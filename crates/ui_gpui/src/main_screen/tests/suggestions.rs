//! Suggested sessions: the sidebar's light bulb in front of the real core,
//! with GitHub played by a fake.

use crate::test_support::{MainScreenTest, TestCore};
use code_assistant_core::mocks::MockLLMProvider;
use code_assistant_core::suggestions::query::search_query;
use code_assistant_core::suggestions::source::{FakeGitHub, GitHubSource};
use code_assistant_core::suggestions::{SuggestionEnv, SuggestionsSnapshot};
use code_assistant_core::ui::UiEvent;
use github::RepoRef;
use github::model::{IssueDetail, IssueSummary};
use gpui_kit::TestAppContext;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const TEMPLATE: &str = r#"---
name: architect
subject: issue
title: "Plan #{{issue.number}}"
suggest:
  - when: issues
    assignee: me
---
Write a plan for {{issue.url}}."#;

fn git(dir: &Path, args: &[&str]) {
    let status = process_env::std_command("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=T",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn issue() -> IssueSummary {
    IssueSummary {
        repo: RepoRef::new("github.com", "acme", "shop"),
        number: 42,
        title: "Coupons".into(),
        url: "https://github.com/acme/shop/issues/42".into(),
        author: Some("maria".into()),
        labels: vec![],
        assignees: vec!["dk".into()],
        is_pull_request: false,
        is_draft: false,
        head_branch: None,
        base_branch: None,
        node_id: "I_42".into(),
        updated_at: chrono::Utc::now(),
    }
}

/// A project folder with one repository on acme/shop, a template that
/// suggests itself for issues assigned to me, and GitHub knowing one.
fn suggest_one(dir: &Path) -> SuggestionEnv {
    let project = dir.join("shop");
    std::fs::create_dir_all(&project).unwrap();
    git(&project, &["init", "-q"]);
    git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/shop.git",
        ],
    );
    let templates = dir.join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    std::fs::write(templates.join("architect.md"), TEMPLATE).unwrap();

    let rule = code_assistant_core::session_templates::parse_template(
        TEMPLATE,
        "architect",
        Path::new("/t"),
    )
    .unwrap()
    .suggest
    .remove(0);
    let mut fake = FakeGitHub {
        login: "dk".into(),
        ..Default::default()
    };
    fake.searches.lock().unwrap().insert(
        (
            "github.com".into(),
            search_query(&rule, &[issue().repo], chrono::Utc::now()),
        ),
        vec![issue()],
    );
    fake.issues.push(IssueDetail {
        summary: issue(),
        body: String::new(),
    });
    let fake = Arc::new(fake);
    SuggestionEnv {
        templates_dir: templates,
        state_path: None,
        projects: Arc::new(move || vec![("shop".to_string(), PathBuf::from(&project))]),
        source: Arc::new(move |_| fake.clone() as Arc<dyn GitHubSource>),
    }
}

#[gpui_kit::test]
fn no_suggestions_no_light_bulb(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    assert!(!test.shows("suggestions-button"));
    test.push(UiEvent::SuggestionsChanged {
        snapshot: SuggestionsSnapshot {
            problems: vec!["github.com: no GitHub token".into()],
            ..Default::default()
        },
    });
    assert!(
        !test.shows("suggestions-button"),
        "problems alone show nothing"
    );
}

#[gpui_kit::test]
fn a_suggestion_starts_a_session_with_its_message_in_the_input(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let core = TestCore::new(MockLLMProvider::new(vec![]).streaming().into_factory());
    core.service.set_suggestion_env(suggest_one(dir.path()));
    let mut test = MainScreenTest::with_core(cx, core);

    let service = test.core.as_ref().unwrap().service.clone();
    test.core
        .as_ref()
        .unwrap()
        .block_on(service.refresh_suggestions())
        .unwrap();
    test.wait_until("the light bulb shows, lit", |test| {
        test.shows("suggestions-button") && test.gpui.suggestions_snapshot().has_unseen()
    });

    // Opening the list marks what is in it as seen.
    test.click("suggestions-button");
    test.wait_until("the suggestion was seen", |test| {
        !test.gpui.suggestions_snapshot().has_unseen()
    });
    assert!(test.shows("suggestion-row"));

    test.click("suggestion-row");
    test.wait_until("the new session shows its first message", |test| {
        test.gpui.get_current_session_id().is_some()
            && test.input_text() == "Write a plan for https://github.com/acme/shop/issues/42."
    });
    test.wait_until("the suggestion left the list", |test| {
        test.gpui.suggestions_snapshot().suggestions.is_empty()
    });
    assert!(!test.shows("suggestions-button"));
    assert!(test.transcript().is_empty(), "nothing was sent");
}
