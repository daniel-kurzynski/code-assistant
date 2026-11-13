//! Issue searches, single issues and board placements, through the scripted
//! transport.

use github::RepoRef;
use github::fake::FakeTransport;
use serde_json::json;

fn issue_row(number: u64) -> serde_json::Value {
    json!({
        "__typename": "Issue", "id": format!("I_{number}"), "number": number,
        "title": "Coupons", "url": format!("https://ghe.corp/acme/shop/issues/{number}"),
        "updatedAt": "2026-11-13T10:00:00Z",
        "author": {"__typename": "User", "login": "maria"},
        "repository": {"nameWithOwner": "acme/shop"},
        "labels": {"nodes": [{"name": "agent", "color": "ff0000"}]},
        "assignees": {"nodes": [{"login": "dk"}]}
    })
}

#[tokio::test]
async fn a_search_reads_issues_and_pull_requests_on_an_enterprise_host() {
    let fake = FakeTransport::new();
    fake.on_graphql("IssueSearch")
        .data(json!({ "search": { "nodes": [
            issue_row(42),
            { "__typename": "PullRequest", "id": "PR_7", "number": 7, "title": "Fix",
              "url": "https://ghe.corp/acme/shop/pull/7", "updatedAt": "2026-11-12T10:00:00Z",
              "isDraft": true, "headRefName": "fix", "baseRefName": "main",
              "repository": {"nameWithOwner": "acme/shop"} },
            { "__typename": "Discussion", "id": "D_1" },
            null
        ]}}));
    let found = fake
        .client()
        .search_issues("GHE.corp", "is:open assignee:@me", 500)
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].repo, RepoRef::new("ghe.corp", "acme", "shop"));
    assert_eq!(found[0].labels, ["agent"]);
    assert_eq!(found[0].assignees, ["dk"]);
    assert_eq!(found[0].author.as_deref(), Some("maria"));
    assert!(!found[0].is_pull_request);
    assert!(found[1].is_pull_request && found[1].is_draft);
    assert_eq!(found[1].head_branch.as_deref(), Some("fix"));

    let request = &fake.requests()[0];
    assert_eq!(request.url, "https://ghe.corp/api/graphql");
    assert_eq!(
        request.variables,
        Some(json!({ "q": "is:open assignee:@me", "first": 100 }))
    );
}

#[tokio::test]
async fn one_issue_with_its_description() {
    let fake = FakeTransport::new();
    let mut row = issue_row(42);
    row["body"] = "Add coupons".into();
    fake.on_graphql("IssueOrPullRequest")
        .with_variables(json!({ "owner": "acme", "name": "shop", "number": 42 }))
        .data(json!({ "repository": { "issueOrPullRequest": row } }));
    let detail = fake
        .client()
        .issue_or_pull_request(&RepoRef::new("github.com", "acme", "shop"), 42)
        .await
        .unwrap();
    assert_eq!(detail.summary.number, 42);
    assert_eq!(detail.body, "Add coupons");
    assert_eq!(fake.requests()[0].url, "https://api.github.com/graphql");
}

#[tokio::test]
async fn a_missing_issue_is_not_found() {
    let fake = FakeTransport::new();
    fake.on_graphql("IssueOrPullRequest")
        .data(json!({ "repository": { "issueOrPullRequest": null } }));
    let err = fake
        .client()
        .issue_or_pull_request(&RepoRef::new("github.com", "acme", "shop"), 9)
        .await
        .unwrap_err();
    assert!(matches!(err, github::Error::NotFound { .. }), "{err}");
}

#[tokio::test]
async fn board_placements_by_node_id() {
    let fake = FakeTransport::new();
    fake.on_graphql("BoardPlacements").data(json!({ "nodes": [
        { "__typename": "Issue", "id": "I_1", "projectItems": { "nodes": [
            { "project": { "number": 12, "owner": { "__typename": "Organization", "login": "acme" } },
              "fieldValueByName": { "name": "Refinement" } },
            { "project": { "number": 3, "owner": { "__typename": "User", "login": "dk" } },
              "fieldValueByName": null } ] } },
        null
    ]}));
    let found = fake
        .client()
        .board_placements("github.com", &["I_1".into(), "gone".into()], "Status")
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    let places = &found["I_1"];
    assert_eq!(places[0].project_owner, "acme");
    assert_eq!(places[0].project_number, 12);
    assert_eq!(places[0].column.as_deref(), Some("Refinement"));
    assert_eq!(places[1].column, None);
    assert_eq!(
        fake.requests()[0].variables,
        Some(json!({ "ids": ["I_1", "gone"], "field": "Status" }))
    );
}

#[tokio::test]
async fn hosts_that_are_not_host_names_are_refused() {
    let fake = FakeTransport::new();
    let err = fake
        .client()
        .search_issues("evil.com/x", "is:open", 10)
        .await
        .unwrap_err();
    assert!(matches!(err, github::Error::Invalid(_)), "{err}");
    assert!(fake.requests().is_empty());
}
