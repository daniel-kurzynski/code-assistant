//! From templates and projects to candidate suggestions.

use super::query::{reason, search_query};
use super::source::GitHubSource;
use super::{SubjectRef, Suggestion};
use crate::pull_requests::{RepoCheckout, github_checkouts};
use crate::session_templates::{RuleKind, SessionTemplate, SubjectKind};
use chrono::{DateTime, Utc};
use github::RepoRef;
use github::model::IssueSummary;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A project with its GitHub repositories, found as the pull request view
/// finds a session's: the folder's own repository, then the checkouts
/// directly below it (`origin`, else the first GitHub remote).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRepos {
    pub name: String,
    pub root: PathBuf,
    pub repos: Vec<RepoCheckout>,
}

/// A project's GitHub checkouts. Blocking (opens repositories); call from a
/// blocking task.
pub fn discover_project_repos(name: &str, root: &Path) -> ProjectRepos {
    ProjectRepos {
        name: name.to_string(),
        root: root.to_path_buf(),
        repos: github_checkouts(root),
    }
}

/// A suggestion before dismissals and seen marks are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub suggestion: Suggestion,
    /// The subject's last change; a "not now" holds until it moves.
    pub updated_at: String,
}

/// The candidates every rule finds, and what went wrong on the way. A host
/// that fails once is not asked again in the same round.
#[derive(Debug, Default)]
pub struct Computed {
    pub candidates: Vec<Candidate>,
    pub problems: Vec<String>,
}

pub async fn compute(
    templates: &[SessionTemplate],
    projects: &[ProjectRepos],
    source: &dyn GitHubSource,
    now: DateTime<Utc>,
) -> Computed {
    let mut computed = Computed::default();
    let mut failed_hosts: HashSet<String> = HashSet::new();
    let mut searches: HashMap<(String, String), Vec<IssueSummary>> = HashMap::new();
    let mut ids: HashSet<String> = HashSet::new();

    for template in templates.iter().filter(|t| !t.suggest.is_empty()) {
        let kind = match template.subject {
            SubjectKind::Issue => SubjectKind::Issue,
            SubjectKind::PullRequest => SubjectKind::PullRequest,
            SubjectKind::None => continue,
        };
        for project in projects.iter().filter(|p| template.applies_to(&p.name)) {
            for rule in &template.suggest {
                let mut by_host: BTreeMap<&str, Vec<RepoRef>> = BTreeMap::new();
                for local in &project.repos {
                    let wanted = rule.repos.is_empty()
                        || rule
                            .repos
                            .iter()
                            .any(|r| r.eq_ignore_ascii_case(&local.repo.full_name()));
                    if wanted {
                        by_host
                            .entry(local.repo.host.as_str())
                            .or_default()
                            .push(local.repo.clone());
                    }
                }
                for (host, mut repos) in by_host {
                    if failed_hosts.contains(host) {
                        continue;
                    }
                    repos.sort_by_key(|r| r.to_string());
                    repos.dedup();
                    let query = search_query(rule, &repos, now);
                    let key = (host.to_string(), query.clone());
                    let mut items = match searches.get(&key) {
                        Some(items) => items.clone(),
                        None => match source.search(host, &query).await {
                            Ok(items) => {
                                searches.insert(key, items.clone());
                                items
                            }
                            Err(e) => {
                                computed.problems.push(format!("{host}: {e:#}"));
                                failed_hosts.insert(host.to_string());
                                continue;
                            }
                        },
                    };
                    // The search API answers with both kinds for some
                    // queries; keep what the rule asks for.
                    items.retain(|item| {
                        item.is_pull_request == (rule.when == RuleKind::PullRequests)
                    });
                    if let Some(board) = &rule.board {
                        let Ok((owner, number)) = board.project_ref() else {
                            continue;
                        };
                        let node_ids: Vec<String> =
                            items.iter().map(|i| i.node_id.clone()).collect();
                        if node_ids.is_empty() {
                            continue;
                        }
                        let placements =
                            match source.board_placements(host, &node_ids, &board.field).await {
                                Ok(placements) => placements,
                                Err(e) => {
                                    computed.problems.push(format!(
                                        "{}: board {}: {e:#}",
                                        template.name, board.project
                                    ));
                                    continue;
                                }
                            };
                        items.retain(|item| {
                            placements.get(&item.node_id).is_some_and(|places| {
                                places.iter().any(|p| {
                                    p.project_owner.eq_ignore_ascii_case(&owner)
                                        && p.project_number == number
                                        && p.column
                                            .as_deref()
                                            .is_some_and(|c| c.eq_ignore_ascii_case(&board.column))
                                })
                            })
                        });
                    }
                    for item in items {
                        let id = suggestion_id(&project.name, &template.name, &item.url);
                        if !ids.insert(id.clone()) {
                            continue;
                        }
                        computed.candidates.push(Candidate {
                            updated_at: item.updated_at.to_rfc3339(),
                            suggestion: Suggestion {
                                id,
                                project: project.name.clone(),
                                template: template.name.clone(),
                                template_description: template.description.clone(),
                                subject: SubjectRef {
                                    kind,
                                    repo: item.repo.clone(),
                                    number: item.number,
                                    title: item.title.clone(),
                                    url: item.url.clone(),
                                },
                                reason: reason(rule),
                                seen: false,
                            },
                        });
                    }
                }
            }
        }
    }
    computed
}

/// Stable per project, template and subject, so dismissals and seen marks
/// survive refreshes and restarts.
pub fn suggestion_id(project: &str, template: &str, subject_url: &str) -> String {
    format!("{project}|{template}|{subject_url}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_templates::parse_template;
    use crate::suggestions::source::FakeGitHub;
    use github::model::BoardPlacement;

    fn template(content: &str) -> SessionTemplate {
        parse_template(content, "t", Path::new("/t.md")).unwrap()
    }

    fn issue(repo: &RepoRef, number: u64, node: &str) -> IssueSummary {
        IssueSummary {
            repo: repo.clone(),
            number,
            title: format!("Issue {number}"),
            url: format!("https://{}/{}/issues/{number}", repo.host, repo.full_name()),
            author: Some("maria".into()),
            labels: vec!["agent".into()],
            assignees: vec!["me".into()],
            is_pull_request: false,
            is_draft: false,
            head_branch: None,
            base_branch: None,
            node_id: node.into(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn project(name: &str, repos: &[RepoRef]) -> ProjectRepos {
        ProjectRepos {
            name: name.into(),
            root: PathBuf::from(format!("/p/{name}")),
            repos: repos
                .iter()
                .map(|r| RepoCheckout {
                    path: PathBuf::from(format!("/p/{name}/{}", r.name)),
                    repo: r.clone(),
                    branch: None,
                })
                .collect(),
        }
    }

    const ARCHITECT: &str = "---\nname: architect\ndescription: Plan it\nsubject: issue\nsuggest:\n  - when: issues\n    assignee: me\n    labels_all: [agent]\n---\nPlan {{issue.url}}";

    #[tokio::test]
    async fn one_search_per_host_covers_a_folder_of_repositories() {
        let api = RepoRef::new("github.com", "acme", "api");
        let web = RepoRef::new("github.com", "acme", "web");
        let internal = RepoRef::new("ghe.corp", "team", "infra");
        let fake = FakeGitHub::default();
        let now = Utc::now();
        let rule = &template(ARCHITECT).suggest[0];
        fake.searches.lock().unwrap().insert(
            (
                "github.com".into(),
                search_query(rule, &[api.clone(), web.clone()], now),
            ),
            vec![issue(&api, 1, "A"), issue(&web, 2, "B")],
        );
        fake.searches.lock().unwrap().insert(
            (
                "ghe.corp".into(),
                search_query(rule, std::slice::from_ref(&internal), now),
            ),
            vec![issue(&internal, 3, "C")],
        );
        let computed = compute(
            &[template(ARCHITECT)],
            &[project("shop", &[web, api, internal])],
            &fake,
            now,
        )
        .await;
        assert!(computed.problems.is_empty(), "{:?}", computed.problems);
        let numbers: Vec<u64> = computed
            .candidates
            .iter()
            .map(|c| c.suggestion.subject.number)
            .collect();
        // Hosts in name order, then the search's order.
        assert_eq!(numbers, vec![3, 1, 2]);
        let first = &computed.candidates[1].suggestion;
        assert_eq!(first.project, "shop");
        assert_eq!(first.template, "architect");
        assert_eq!(first.reason, "assigned to you · agent");
        assert_eq!(fake.asked.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_failing_host_is_reported_once_and_the_others_still_count() {
        let good = RepoRef::new("github.com", "acme", "api");
        let bad = RepoRef::new("ghe.corp", "team", "infra");
        let mut fake = FakeGitHub::default();
        fake.failing_hosts
            .insert("ghe.corp".into(), "no GitHub token for ghe.corp".into());
        let now = Utc::now();
        let rule = &template(ARCHITECT).suggest[0];
        fake.searches.lock().unwrap().insert(
            (
                "github.com".into(),
                search_query(rule, std::slice::from_ref(&good), now),
            ),
            vec![issue(&good, 1, "A")],
        );
        let computed = compute(
            &[template(ARCHITECT)],
            &[
                project("a", &[good.clone(), bad.clone()]),
                project("b", &[bad]),
            ],
            &fake,
            now,
        )
        .await;
        assert_eq!(computed.candidates.len(), 1);
        assert_eq!(
            computed.problems,
            vec!["ghe.corp: no GitHub token for ghe.corp"]
        );
    }

    #[tokio::test]
    async fn the_board_column_filters_after_the_search() {
        let repo = RepoRef::new("github.com", "acme", "api");
        let with_board = "---\nname: architect\nsubject: issue\nsuggest:\n  - when: issues\n    board: { project: acme/12, column: refinement }\n---\nHi";
        let mut fake = FakeGitHub::default();
        let now = Utc::now();
        let rule = &template(with_board).suggest[0];
        fake.searches.lock().unwrap().insert(
            (
                "github.com".into(),
                search_query(rule, std::slice::from_ref(&repo), now),
            ),
            vec![
                issue(&repo, 1, "A"),
                issue(&repo, 2, "B"),
                issue(&repo, 3, "C"),
            ],
        );
        let place = |owner: &str, number, column: &str| BoardPlacement {
            project_owner: owner.into(),
            project_number: number,
            column: Some(column.into()),
        };
        fake.boards
            .insert("A".into(), vec![place("acme", 12, "Refinement")]);
        fake.boards
            .insert("B".into(), vec![place("acme", 12, "Implementation")]);
        fake.boards
            .insert("C".into(), vec![place("other", 12, "Refinement")]);
        let computed = compute(
            &[template(with_board)],
            &[project("p", &[repo])],
            &fake,
            now,
        )
        .await;
        let numbers: Vec<u64> = computed
            .candidates
            .iter()
            .map(|c| c.suggestion.subject.number)
            .collect();
        assert_eq!(numbers, vec![1]);
    }

    #[tokio::test]
    async fn templates_limited_to_projects_and_rules_limited_to_repos() {
        let api = RepoRef::new("github.com", "acme", "api");
        let web = RepoRef::new("github.com", "acme", "web");
        let limited = "---\nname: x\nsubject: issue\nprojects: [shop]\nsuggest:\n  - when: issues\n    repos: [ACME/web]\n---\nHi";
        let fake = FakeGitHub::default();
        compute(
            &[template(limited)],
            &[
                project("shop", &[api.clone(), web.clone()]),
                project("pal", &[api]),
            ],
            &fake,
            Utc::now(),
        )
        .await;
        let asked = fake.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].contains("repo:acme/web"), "{}", asked[0]);
        assert!(!asked[0].contains("repo:acme/api"), "{}", asked[0]);
    }
}
