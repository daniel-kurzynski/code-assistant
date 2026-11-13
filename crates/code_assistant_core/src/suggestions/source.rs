//! Where suggestions read GitHub from: the client the pull request view
//! uses, or a fake in tests.

use anyhow::Result;
use async_trait::async_trait;
use github::RepoRef;
use github::model::{BoardPlacement, IssueDetail, IssueSummary};
use std::collections::HashMap;

/// The GitHub reads suggestions and template starts need.
#[async_trait]
pub trait GitHubSource: Send + Sync {
    /// The login the host's credential signs in as.
    async fn login(&self, host: &str) -> Result<String>;
    async fn search(&self, host: &str, query: &str) -> Result<Vec<IssueSummary>>;
    async fn board_placements(
        &self,
        host: &str,
        node_ids: &[String],
        field: &str,
    ) -> Result<HashMap<String, Vec<BoardPlacement>>>;
    async fn issue(&self, repo: &RepoRef, number: u64) -> Result<IssueDetail>;
}

/// How many results one rule search reads.
const SEARCH_LIMIT: u32 = 50;

/// The shared [`github::Client`]: the pull request view's token lookup,
/// host endpoints (github.com, GHE.com, Enterprise servers), rate limits
/// and request slots apply to suggestions too.
pub struct LiveGitHub {
    client: github::Client,
}

impl LiveGitHub {
    pub fn new(client: github::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl GitHubSource for LiveGitHub {
    async fn login(&self, host: &str) -> Result<String> {
        Ok(self.client.viewer(host).await?.login)
    }

    async fn search(&self, host: &str, query: &str) -> Result<Vec<IssueSummary>> {
        Ok(self.client.search_issues(host, query, SEARCH_LIMIT).await?)
    }

    async fn board_placements(
        &self,
        host: &str,
        node_ids: &[String],
        field: &str,
    ) -> Result<HashMap<String, Vec<BoardPlacement>>> {
        self.client
            .board_placements(host, node_ids, field)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "{e} (boards need the read:project scope: \
                     `gh auth refresh --hostname {host} --scopes read:project`)"
                )
            })
    }

    async fn issue(&self, repo: &RepoRef, number: u64) -> Result<IssueDetail> {
        Ok(self.client.issue_or_pull_request(repo, number).await?)
    }
}

/// A scripted GitHub for tests: search answers by query, issues by number.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Default)]
pub struct FakeGitHub {
    pub login: String,
    /// `(host, query)` → results; an unknown query finds nothing.
    pub searches: std::sync::Mutex<HashMap<(String, String), Vec<IssueSummary>>>,
    /// Hosts whose every call fails with this message.
    pub failing_hosts: HashMap<String, String>,
    pub boards: HashMap<String, Vec<BoardPlacement>>,
    pub issues: Vec<IssueDetail>,
    /// Every search query asked, in order.
    pub asked: std::sync::Mutex<Vec<String>>,
}

#[cfg(any(test, feature = "test-utils"))]
impl FakeGitHub {
    fn check(&self, host: &str) -> Result<()> {
        match self.failing_hosts.get(host) {
            Some(message) => anyhow::bail!("{message}"),
            None => Ok(()),
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait]
impl GitHubSource for FakeGitHub {
    async fn login(&self, host: &str) -> Result<String> {
        self.check(host)?;
        Ok(self.login.clone())
    }

    async fn search(&self, host: &str, query: &str) -> Result<Vec<IssueSummary>> {
        self.check(host)?;
        self.asked.lock().unwrap().push(query.to_string());
        Ok(self
            .searches
            .lock()
            .unwrap()
            .get(&(host.to_string(), query.to_string()))
            .cloned()
            .unwrap_or_default())
    }

    async fn board_placements(
        &self,
        host: &str,
        node_ids: &[String],
        _field: &str,
    ) -> Result<HashMap<String, Vec<BoardPlacement>>> {
        self.check(host)?;
        Ok(node_ids
            .iter()
            .filter_map(|id| self.boards.get(id).map(|b| (id.clone(), b.clone())))
            .collect())
    }

    async fn issue(&self, repo: &RepoRef, number: u64) -> Result<IssueDetail> {
        self.check(&repo.host)?;
        self.issues
            .iter()
            .find(|d| d.summary.repo == *repo && d.summary.number == number)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no issue {repo}#{number}"))
    }
}
