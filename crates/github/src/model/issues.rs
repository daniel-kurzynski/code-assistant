//! Issues and pull requests as a search lists them, and their places on
//! project boards.

use crate::RepoRef;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An issue or pull request found by [`crate::Client::search_issues`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueSummary {
    pub repo: RepoRef,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub is_pull_request: bool,
    /// Pull requests only.
    pub is_draft: bool,
    /// Pull requests only.
    pub head_branch: Option<String>,
    /// Pull requests only.
    pub base_branch: Option<String>,
    /// GraphQL node id, for [`crate::Client::board_placements`].
    pub node_id: String,
    pub updated_at: DateTime<Utc>,
}

/// An issue or pull request with its description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDetail {
    pub summary: IssueSummary,
    pub body: String,
}

/// Where an issue or pull request sits on a project board (Projects v2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardPlacement {
    /// Login of the user or organization owning the project.
    pub project_owner: String,
    pub project_number: u64,
    /// The value of the single-select field asked for, e.g. the "Status"
    /// column.
    pub column: Option<String>,
}
