//! A suggestion rule as a GitHub search query, and as a reason to show.
//!
//! The search API does most of the filtering (state, repositories,
//! assignee, labels, draft, checks, age). What it cannot express, the board
//! column, is checked afterwards with a GraphQL lookup.

use crate::session_templates::{ChecksFilter, RuleKind, SuggestRule};
use chrono::{DateTime, Duration, Utc};
use github::RepoRef;

/// The search query for `rule` over `repos` (all on one host).
pub fn search_query(rule: &SuggestRule, repos: &[RepoRef], now: DateTime<Utc>) -> String {
    let mut terms: Vec<String> = vec!["is:open".into()];
    terms.push(
        match rule.when {
            RuleKind::Issues => "is:issue",
            RuleKind::PullRequests => "is:pr",
        }
        .into(),
    );
    for repo in repos {
        terms.push(format!("repo:{}", repo.full_name()));
    }
    if let Some(who) = &rule.assignee {
        match who.as_str() {
            "any" => {}
            "none" => terms.push("no:assignee".into()),
            other => terms.push(format!("assignee:{}", user(other))),
        }
    }
    if let Some(who) = &rule.author {
        terms.push(format!("author:{}", user(who)));
    }
    if let Some(who) = &rule.review_requested {
        terms.push(format!("review-requested:{}", user(who)));
    }
    for label in &rule.labels_all {
        terms.push(format!("label:{}", quoted(label)));
    }
    if !rule.labels_any.is_empty() {
        let any: Vec<String> = rule.labels_any.iter().map(|l| quoted(l)).collect();
        terms.push(format!("label:{}", any.join(",")));
    }
    for label in &rule.labels_none {
        terms.push(format!("-label:{}", quoted(label)));
    }
    if rule.when == RuleKind::PullRequests {
        terms.push(format!("draft:{}", rule.draft.unwrap_or(false)));
    }
    if let Some(checks) = rule.checks {
        terms.push(
            match checks {
                ChecksFilter::Failing => "status:failure",
                ChecksFilter::Passing => "status:success",
                ChecksFilter::Pending => "status:pending",
            }
            .into(),
        );
    }
    if let Some(days) = rule.updated_within_days {
        let since = now - Duration::days(i64::from(days));
        terms.push(format!("updated:>={}", since.format("%Y-%m-%d")));
    }
    terms.join(" ")
}

fn user(who: &str) -> String {
    if who == "me" {
        "@me".into()
    } else {
        who.trim_start_matches('@').to_string()
    }
}

fn quoted(label: &str) -> String {
    if label.chars().any(|c| c.is_whitespace() || c == ',') {
        format!("\"{}\"", label.replace('"', ""))
    } else {
        label.to_string()
    }
}

/// Why a rule offers a subject, in a few words: "assigned to you · agent ·
/// Refinement".
pub fn reason(rule: &SuggestRule) -> String {
    let mut parts: Vec<String> = Vec::new();
    match rule.assignee.as_deref() {
        Some("me") => parts.push("assigned to you".into()),
        Some("none") => parts.push("unassigned".into()),
        Some("any") | None => {}
        Some(other) => parts.push(format!("assigned to {other}")),
    }
    match rule.author.as_deref() {
        Some("me") => parts.push(match rule.when {
            RuleKind::Issues => "opened by you".into(),
            RuleKind::PullRequests => "your pull request".into(),
        }),
        Some(other) => parts.push(format!("by {other}")),
        None => {}
    }
    match rule.review_requested.as_deref() {
        Some("me") => parts.push("review requested from you".into()),
        Some(other) => parts.push(format!("review requested from {other}")),
        None => {}
    }
    parts.extend(rule.labels_all.iter().cloned());
    if !rule.labels_any.is_empty() {
        parts.push(rule.labels_any.join(" or "));
    }
    if let Some(board) = &rule.board {
        parts.push(board.column.clone());
    }
    match rule.checks {
        Some(ChecksFilter::Failing) => parts.push("checks failing".into()),
        Some(ChecksFilter::Passing) => parts.push("checks passing".into()),
        Some(ChecksFilter::Pending) => parts.push("checks running".into()),
        None => {}
    }
    if parts.is_empty() {
        parts.push(match rule.when {
            RuleKind::Issues => "open issue".into(),
            RuleKind::PullRequests => "open pull request".into(),
        });
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_templates::BoardFilter;
    use chrono::TimeZone;

    fn rule(when: RuleKind) -> SuggestRule {
        SuggestRule {
            when,
            repos: vec![],
            assignee: None,
            author: None,
            review_requested: None,
            labels_all: vec![],
            labels_any: vec![],
            labels_none: vec![],
            board: None,
            draft: None,
            checks: None,
            updated_within_days: None,
        }
    }

    fn repos() -> Vec<RepoRef> {
        vec![
            RepoRef::new("github.com", "acme", "shop-api"),
            RepoRef::new("github.com", "acme", "shop-web"),
        ]
    }

    #[test]
    fn an_issue_rule_with_every_filter() {
        let mut r = rule(RuleKind::Issues);
        r.assignee = Some("me".into());
        r.labels_all = vec!["agent".into(), "needs plan".into()];
        r.labels_any = vec!["bug".into(), "feature".into()];
        r.labels_none = vec!["wontfix".into()];
        r.updated_within_days = Some(14);
        let now = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        assert_eq!(
            search_query(&r, &repos(), now),
            "is:open is:issue repo:acme/shop-api repo:acme/shop-web assignee:@me \
             label:agent label:\"needs plan\" label:bug,feature -label:wontfix \
             updated:>=2026-10-30"
        );
    }

    #[test]
    fn pull_request_rules_skip_drafts_unless_asked() {
        let mut r = rule(RuleKind::PullRequests);
        r.review_requested = Some("me".into());
        r.checks = Some(ChecksFilter::Failing);
        let q = search_query(&r, &repos()[..1], Utc::now());
        assert_eq!(
            q,
            "is:open is:pr repo:acme/shop-api review-requested:@me draft:false status:failure"
        );
        r.draft = Some(true);
        assert!(search_query(&r, &repos()[..1], Utc::now()).contains("draft:true"));
    }

    #[test]
    fn assignee_none_and_named_users() {
        let mut r = rule(RuleKind::Issues);
        r.assignee = Some("none".into());
        r.author = Some("@maria".into());
        let q = search_query(&r, &[], Utc::now());
        assert_eq!(q, "is:open is:issue no:assignee author:maria");
    }

    #[test]
    fn reasons_read_like_the_rule() {
        let mut r = rule(RuleKind::Issues);
        r.assignee = Some("me".into());
        r.labels_all = vec!["agent".into()];
        r.board = Some(BoardFilter {
            project: "acme/12".into(),
            column: "Refinement".into(),
            field: "Status".into(),
        });
        assert_eq!(reason(&r), "assigned to you · agent · Refinement");
        let mut pr = rule(RuleKind::PullRequests);
        pr.author = Some("me".into());
        pr.checks = Some(ChecksFilter::Failing);
        assert_eq!(reason(&pr), "your pull request · checks failing");
        assert_eq!(reason(&rule(RuleKind::Issues)), "open issue");
    }
}
