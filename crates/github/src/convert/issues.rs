//! Issue search rows, single issues and board placements.

use super::{Connection, RawActor, RawLabel, trimmed};
use crate::RepoRef;
use crate::model::{BoardPlacement, IssueDetail, IssueSummary};
use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawRepository {
    pub name_with_owner: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawLogin {
    pub login: Option<String>,
}

/// An issue or pull request row (`IssueRow`, `PullRequestIssueRow`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawIssue {
    #[serde(rename = "__typename")]
    pub typename: Option<String>,
    pub id: Option<String>,
    pub number: Option<u64>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    pub is_draft: Option<bool>,
    pub head_ref_name: Option<String>,
    pub base_ref_name: Option<String>,
    pub author: Option<RawActor>,
    pub repository: Option<RawRepository>,
    pub labels: Connection<RawLabel>,
    pub assignees: Connection<RawLogin>,
    pub body: Option<String>,
}

/// A row, or `None` when it is neither an issue nor a pull request or lacks
/// what identifies it.
pub(crate) fn issue_summary(host: &str, raw: &RawIssue) -> Option<IssueSummary> {
    let is_pull_request = match raw.typename.as_deref() {
        Some("Issue") => false,
        Some("PullRequest") => true,
        _ => return None,
    };
    let name_with_owner = raw.repository.as_ref()?.name_with_owner.as_deref()?;
    let (owner, name) = name_with_owner.split_once('/')?;
    Some(IssueSummary {
        repo: RepoRef::new(host, owner, name),
        number: raw.number?,
        title: raw.title.clone().unwrap_or_default(),
        url: trimmed(raw.url.as_deref())?,
        author: raw
            .author
            .as_ref()
            .and_then(|a| trimmed(a.login.as_deref())),
        labels: raw
            .labels
            .nodes()
            .filter_map(|l| trimmed(l.name.as_deref()))
            .collect(),
        assignees: raw
            .assignees
            .nodes()
            .filter_map(|a| trimmed(a.login.as_deref()))
            .collect(),
        is_pull_request,
        is_draft: raw.is_draft.unwrap_or(false),
        head_branch: trimmed(raw.head_ref_name.as_deref()),
        base_branch: trimmed(raw.base_ref_name.as_deref()),
        node_id: trimmed(raw.id.as_deref())?,
        updated_at: raw.updated_at.unwrap_or_default(),
    })
}

pub(crate) fn issue_detail(host: &str, raw: &RawIssue) -> Option<IssueDetail> {
    Some(IssueDetail {
        summary: issue_summary(host, raw)?,
        body: raw.body.clone().unwrap_or_default(),
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawProject {
    pub number: Option<u64>,
    pub owner: Option<RawLogin>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawFieldValue {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawProjectItem {
    pub project: Option<RawProject>,
    pub field_value_by_name: Option<RawFieldValue>,
}

/// One node of `BoardPlacements`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawPlacedNode {
    pub id: Option<String>,
    pub project_items: Connection<RawProjectItem>,
}

pub(crate) fn placements(raw: &RawPlacedNode) -> Vec<BoardPlacement> {
    raw.project_items
        .nodes()
        .filter_map(|item| {
            let project = item.project.as_ref()?;
            Some(BoardPlacement {
                project_owner: project
                    .owner
                    .as_ref()
                    .and_then(|o| trimmed(o.login.as_deref()))
                    .unwrap_or_default(),
                project_number: project.number?,
                column: item
                    .field_value_by_name
                    .as_ref()
                    .and_then(|v| trimmed(v.name.as_deref())),
            })
        })
        .collect()
}
