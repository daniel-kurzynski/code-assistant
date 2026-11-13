//! Issue searches, single issues and board placements, for suggested
//! sessions.

use super::ensure_addressable;
use crate::RepoRef;
use crate::client::Client;
use crate::convert::issues::{RawIssue, RawPlacedNode, issue_detail, issue_summary, placements};
use crate::convert::{Connection, decode};
use crate::error::{Error, Result};
use crate::model::{BoardPlacement, IssueDetail, IssueSummary};
use crate::repo::{is_github_host, normalize_host};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

/// `search` refuses `first` above this.
const MAX_ROWS: u32 = 100;
/// `nodes` takes at most this many ids.
const MAX_NODES: usize = 100;

fn ensure_host(host: &str) -> Result<String> {
    let host = normalize_host(host);
    if is_github_host(&host) {
        Ok(host)
    } else {
        Err(Error::Invalid(format!("{host} is not a GitHub host")))
    }
}

impl Client {
    /// Issues and pull requests on `host` matching a search query
    /// (`is:open is:issue repo:o/n assignee:@me …`), at most `limit`, in
    /// the order GitHub ranks them. Rows that are neither are dropped.
    pub async fn search_issues(
        &self,
        host: &str,
        query: &str,
        limit: u32,
    ) -> Result<Vec<IssueSummary>> {
        #[derive(Deserialize)]
        struct Data {
            search: Connection<RawIssue>,
        }
        let host = ensure_host(host)?;
        let variables = json!({ "q": query, "first": limit.clamp(1, MAX_ROWS) });
        let data = self.graphql(&host, "IssueSearch", variables).await?;
        let data: Data = decode("IssueSearch", data)?;
        Ok(data
            .search
            .nodes()
            .filter_map(|raw| issue_summary(&host, raw))
            .collect())
    }

    /// One issue or pull request with its description.
    pub async fn issue_or_pull_request(&self, repo: &RepoRef, number: u64) -> Result<IssueDetail> {
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct Repository {
            issue_or_pull_request: Option<RawIssue>,
        }
        #[derive(Deserialize)]
        struct Data {
            repository: Option<Repository>,
        }
        ensure_addressable(repo)?;
        if number == 0 || number > i32::MAX as u64 {
            return Err(Error::Invalid(format!("{number} is not an issue number")));
        }
        let variables = json!({ "owner": repo.owner, "name": repo.name, "number": number });
        let data = self
            .graphql_reserved(&repo.host, "IssueOrPullRequest", variables)
            .await?;
        let data: Data = decode("IssueOrPullRequest", data)?;
        data.repository
            .and_then(|r| r.issue_or_pull_request)
            .and_then(|raw| issue_detail(&repo.host, &raw))
            .ok_or(Error::NotFound {
                operation: "IssueOrPullRequest",
            })
    }

    /// The board placements of issues and pull requests on `host`, by node
    /// id, with the value of the single-select `field` (e.g. "Status").
    /// Needs the `read:project` scope. Ids GitHub does not answer for are
    /// left out.
    pub async fn board_placements(
        &self,
        host: &str,
        node_ids: &[String],
        field: &str,
    ) -> Result<HashMap<String, Vec<BoardPlacement>>> {
        #[derive(Deserialize)]
        struct Data {
            nodes: Vec<Option<RawPlacedNode>>,
        }
        let host = ensure_host(host)?;
        let mut found = HashMap::new();
        for chunk in node_ids.chunks(MAX_NODES) {
            let variables = json!({ "ids": chunk, "field": field });
            let data = self.graphql(&host, "BoardPlacements", variables).await?;
            let data: Data = decode("BoardPlacements", data)?;
            for node in data.nodes.into_iter().flatten() {
                if let Some(id) = node.id.clone() {
                    found.insert(id, placements(&node));
                }
            }
        }
        Ok(found)
    }
}
