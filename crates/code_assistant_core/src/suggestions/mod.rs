//! Suggested sessions: work on GitHub that fits a session template.
//!
//! Templates with `suggest:` rules (see [`crate::session_templates`]) are
//! matched against the open issues and pull requests of every project's
//! repositories. A match is a [`Suggestion`]; the GPUI sidebar shows them
//! behind a light bulb, and one click starts a session from the template
//! with the first message in the input.
//!
//! GitHub is read through the same [`github::Client`] as the pull request
//! view (its token from `GH_TOKEN` / `gh auth token`, its endpoints for
//! github.com, GHE.com and Enterprise servers, its rate limits), and a
//! project's repositories are found as that view finds a session's
//! ([`crate::pull_requests`]): the server comes from each checkout's remote.
//!
//! State kept across restarts (`<data_dir>/suggestions.json`): which
//! suggestions were seen, dismissed (for now or for good) and taken (turned
//! into a session).

pub mod compute;
pub mod query;
pub mod source;
pub mod subject;

use crate::session::SessionService;
use crate::session_templates::SubjectKind;
use crate::utils::file_utils::atomic_write_json;
use compute::Candidate;
use github::RepoRef;
use serde::{Deserialize, Serialize};
use source::{GitHubSource, LiveGitHub};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tracing::{debug, warn};

/// The issue or pull request a suggestion is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectRef {
    pub kind: SubjectKind,
    pub repo: RepoRef,
    pub number: u64,
    pub title: String,
    pub url: String,
}

/// One offer: this template for this subject in this project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Stable across refreshes (`project|template|url`).
    pub id: String,
    pub project: String,
    pub template: String,
    pub template_description: String,
    pub subject: SubjectRef,
    /// Why the rule matched, e.g. "assigned to you · agent · Refinement".
    pub reason: String,
    /// The user has had the list open while this was in it.
    pub seen: bool,
}

/// Everything a frontend shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuggestionsSnapshot {
    /// Visible suggestions: not dismissed, not taken.
    pub suggestions: Vec<Suggestion>,
    /// What went wrong in the last refresh (no token, bad template, …).
    pub problems: Vec<String>,
    pub refreshing: bool,
    pub refreshed_at: Option<SystemTime>,
}

impl SuggestionsSnapshot {
    pub fn has_unseen(&self) -> bool {
        self.suggestions.iter().any(|s| !s.seen)
    }
}

/// How a suggestion is put away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dismissal {
    /// Until the issue or pull request changes.
    NotNow,
    /// For good.
    Never,
}

/// Where suggestions come from. Replaceable for tests.
pub struct SuggestionEnv {
    pub templates_dir: PathBuf,
    /// `None` keeps the state in memory only.
    pub state_path: Option<PathBuf>,
    /// Project names and folders.
    pub projects: Arc<dyn Fn() -> Vec<(String, PathBuf)> + Send + Sync>,
    /// The GitHub reader, given the service's shared client.
    pub source: Arc<dyn Fn(github::Client) -> Arc<dyn GitHubSource> + Send + Sync>,
}

impl SuggestionEnv {
    /// The installed app: templates in the config dir, state in the data
    /// dir, the projects of `projects.json`, GitHub through the shared
    /// client.
    pub fn live() -> Self {
        Self {
            templates_dir: crate::session_templates::templates_dir(),
            state_path: Some(crate::config_dir::data_dir().join("suggestions.json")),
            projects: Arc::new(|| {
                let mut projects: Vec<(String, PathBuf)> = crate::config::load_projects()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(name, project)| (name, project.path))
                    .collect();
                projects.sort();
                projects
            }),
            source: Arc::new(|client| Arc::new(LiveGitHub::new(client)) as Arc<dyn GitHubSource>),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    dismissed: BTreeMap<String, Dismissed>,
    #[serde(default)]
    seen: BTreeSet<String>,
    /// Suggestion id → the session started from it.
    #[serde(default)]
    taken: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Dismissed {
    /// `None`: for good. `Some(t)`: until the subject's `updated_at`
    /// differs from `t`.
    #[serde(default)]
    until_changed_from: Option<String>,
}

#[derive(Default)]
struct Inner {
    loaded: bool,
    persisted: PersistedState,
    candidates: Vec<Candidate>,
    problems: Vec<String>,
    refreshing: bool,
    refreshed_at: Option<SystemTime>,
}

/// The suggestion state a [`SessionService`] holds.
pub struct SuggestionStore {
    env: Mutex<Arc<SuggestionEnv>>,
    inner: Mutex<Inner>,
}

impl Default for SuggestionStore {
    fn default() -> Self {
        Self {
            env: Mutex::new(Arc::new(SuggestionEnv::live())),
            inner: Mutex::new(Inner::default()),
        }
    }
}

impl SuggestionStore {
    pub fn env(&self) -> Arc<SuggestionEnv> {
        self.env.lock().unwrap().clone()
    }

    /// Replace where suggestions come from; forgets the current state.
    pub fn set_env(&self, env: SuggestionEnv) {
        *self.env.lock().unwrap() = Arc::new(env);
        *self.inner.lock().unwrap() = Inner::default();
    }

    fn with_inner<T>(&self, f: impl FnOnce(&mut Inner, &SuggestionEnv) -> T) -> T {
        let env = self.env();
        let mut inner = self.inner.lock().unwrap();
        if !inner.loaded {
            inner.loaded = true;
            if let Some(path) = &env.state_path
                && let Ok(content) = std::fs::read_to_string(path)
            {
                match serde_json::from_str(&content) {
                    Ok(state) => inner.persisted = state,
                    Err(e) => warn!("Ignoring unreadable {}: {e}", path.display()),
                }
            }
        }
        f(&mut inner, &env)
    }

    fn save(inner: &Inner, env: &SuggestionEnv) {
        if let Some(path) = &env.state_path
            && let Err(e) = atomic_write_json(path, &inner.persisted)
        {
            warn!("Cannot save {}: {e:#}", path.display());
        }
    }

    pub fn snapshot(&self) -> SuggestionsSnapshot {
        self.with_inner(|inner, _| {
            let state = &inner.persisted;
            let suggestions = inner
                .candidates
                .iter()
                .filter(|c| !state.taken.contains_key(&c.suggestion.id))
                .filter(|c| match state.dismissed.get(&c.suggestion.id) {
                    None => true,
                    Some(Dismissed {
                        until_changed_from: None,
                    }) => false,
                    Some(Dismissed {
                        until_changed_from: Some(at),
                    }) => *at != c.updated_at,
                })
                .map(|c| Suggestion {
                    seen: state.seen.contains(&c.suggestion.id),
                    ..c.suggestion.clone()
                })
                .collect();
            SuggestionsSnapshot {
                suggestions,
                problems: inner.problems.clone(),
                refreshing: inner.refreshing,
                refreshed_at: inner.refreshed_at,
            }
        })
    }

    /// Mark a refresh as running; `false` when one already is.
    pub fn begin_refresh(&self) -> bool {
        self.with_inner(|inner, _| !std::mem::replace(&mut inner.refreshing, true))
    }

    /// Install a refresh's result. Marks of suggestions that are gone are
    /// pruned only after a refresh without problems (a failing host would
    /// otherwise reset them); taken marks go with their session.
    pub fn finish_refresh(
        &self,
        candidates: Vec<Candidate>,
        problems: Vec<String>,
        live_sessions: Option<HashSet<String>>,
    ) {
        self.with_inner(|inner, env| {
            inner.refreshing = false;
            inner.refreshed_at = Some(SystemTime::now());
            let complete = problems.is_empty();
            let state = &mut inner.persisted;
            if complete {
                let current: HashSet<&str> = candidates
                    .iter()
                    .map(|c| c.suggestion.id.as_str())
                    .collect();
                state.seen.retain(|id| current.contains(id.as_str()));
                state.dismissed.retain(|id, d| {
                    d.until_changed_from.is_none() || current.contains(id.as_str())
                });
            }
            if let Some(live) = live_sessions {
                state.taken.retain(|_, session| live.contains(session));
            }
            inner.candidates = candidates;
            inner.problems = problems;
            Self::save(inner, env);
        })
    }

    /// End a refresh that could not run at all.
    pub fn fail_refresh(&self, problem: String) {
        self.with_inner(|inner, _| {
            inner.refreshing = false;
            inner.refreshed_at = Some(SystemTime::now());
            inner.problems = vec![problem];
        })
    }

    pub fn candidate(&self, id: &str) -> Option<Suggestion> {
        self.with_inner(|inner, _| {
            inner
                .candidates
                .iter()
                .find(|c| c.suggestion.id == id)
                .map(|c| c.suggestion.clone())
        })
    }

    pub fn dismiss(&self, id: &str, how: Dismissal) {
        self.with_inner(|inner, env| {
            let until_changed_from = match how {
                Dismissal::Never => None,
                Dismissal::NotNow => Some(
                    inner
                        .candidates
                        .iter()
                        .find(|c| c.suggestion.id == id)
                        .map(|c| c.updated_at.clone())
                        .unwrap_or_default(),
                ),
            };
            inner
                .persisted
                .dismissed
                .insert(id.to_string(), Dismissed { until_changed_from });
            Self::save(inner, env);
        })
    }

    /// Mark suggestions as seen; `true` when one was not yet.
    pub fn mark_seen(&self, ids: &[String]) -> bool {
        self.with_inner(|inner, env| {
            let mut changed = false;
            for id in ids {
                changed |= inner.persisted.seen.insert(id.clone());
            }
            if changed {
                Self::save(inner, env);
            }
            changed
        })
    }

    pub fn mark_taken(&self, id: &str, session_id: &str) {
        self.with_inner(|inner, env| {
            inner
                .persisted
                .taken
                .insert(id.to_string(), session_id.to_string());
            Self::save(inner, env);
        })
    }
}

/// How often suggestions are refreshed while the app runs.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Refresh now and then every [`REFRESH_INTERVAL`]. Wiring layers spawn
/// this on the backend runtime of a frontend that shows suggestions.
pub async fn run_suggestion_refresher(service: SessionService) {
    loop {
        if let Err(e) = service.refresh_suggestions().await {
            debug!("Suggestion refresh failed: {e:#}");
        }
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, updated_at: &str) -> Candidate {
        Candidate {
            updated_at: updated_at.into(),
            suggestion: Suggestion {
                id: id.into(),
                project: "p".into(),
                template: "t".into(),
                template_description: String::new(),
                subject: SubjectRef {
                    kind: SubjectKind::Issue,
                    repo: RepoRef::new("github.com", "o", "r"),
                    number: 1,
                    title: "T".into(),
                    url: "u".into(),
                },
                reason: "r".into(),
                seen: false,
            },
        }
    }

    fn store(path: Option<PathBuf>) -> SuggestionStore {
        let store = SuggestionStore::default();
        store.set_env(SuggestionEnv {
            templates_dir: PathBuf::from("/none"),
            state_path: path,
            projects: Arc::new(Vec::new),
            source: Arc::new(|client| Arc::new(LiveGitHub::new(client)) as Arc<dyn GitHubSource>),
        });
        store
    }

    fn ids(store: &SuggestionStore) -> Vec<String> {
        store
            .snapshot()
            .suggestions
            .into_iter()
            .map(|s| s.id)
            .collect()
    }

    #[test]
    fn not_now_holds_until_the_subject_changes() {
        let store = store(None);
        store.finish_refresh(
            vec![candidate("a", "t1"), candidate("b", "t1")],
            vec![],
            None,
        );
        store.dismiss("a", Dismissal::NotNow);
        store.dismiss("b", Dismissal::Never);
        assert!(ids(&store).is_empty());
        store.finish_refresh(
            vec![candidate("a", "t2"), candidate("b", "t2")],
            vec![],
            None,
        );
        assert_eq!(ids(&store), vec!["a"]);
    }

    #[test]
    fn seen_marks_and_the_unseen_flag() {
        let store = store(None);
        store.finish_refresh(vec![candidate("a", "t"), candidate("b", "t")], vec![], None);
        assert!(store.snapshot().has_unseen());
        assert!(store.mark_seen(&["a".into(), "b".into()]));
        assert!(!store.mark_seen(&["a".into()]));
        assert!(!store.snapshot().has_unseen());
        // A new one makes the list unseen again.
        store.finish_refresh(
            vec![
                candidate("a", "t"),
                candidate("b", "t"),
                candidate("c", "t"),
            ],
            vec![],
            None,
        );
        let snapshot = store.snapshot();
        assert!(snapshot.has_unseen());
        assert!(snapshot.suggestions[0].seen);
    }

    #[test]
    fn taken_suggestions_return_when_their_session_is_gone() {
        let store = store(None);
        store.finish_refresh(vec![candidate("a", "t")], vec![], None);
        store.mark_taken("a", "s1");
        assert!(ids(&store).is_empty());
        store.finish_refresh(
            vec![candidate("a", "t")],
            vec![],
            Some(HashSet::from(["s1".to_string()])),
        );
        assert!(ids(&store).is_empty());
        store.finish_refresh(vec![candidate("a", "t")], vec![], Some(HashSet::new()));
        assert_eq!(ids(&store), vec!["a"]);
    }

    #[test]
    fn a_refresh_with_problems_keeps_marks_of_missing_suggestions() {
        let store = store(None);
        store.finish_refresh(vec![candidate("a", "t")], vec![], None);
        store.mark_seen(&["a".into()]);
        store.finish_refresh(vec![], vec!["ghe.corp: no token".into()], None);
        store.finish_refresh(vec![candidate("a", "t")], vec![], None);
        assert!(store.snapshot().suggestions[0].seen);
        store.finish_refresh(vec![], vec![], None);
        store.finish_refresh(vec![candidate("a", "t")], vec![], None);
        assert!(!store.snapshot().suggestions[0].seen);
    }

    #[test]
    fn state_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suggestions.json");
        let first = store(Some(path.clone()));
        first.finish_refresh(vec![candidate("a", "t"), candidate("b", "t")], vec![], None);
        first.dismiss("a", Dismissal::Never);
        first.mark_seen(&["b".into()]);

        let second = store(Some(path));
        second.finish_refresh(vec![candidate("a", "t"), candidate("b", "t")], vec![], None);
        let snapshot = second.snapshot();
        assert_eq!(snapshot.suggestions.len(), 1);
        assert!(snapshot.suggestions[0].seen);
    }

    #[test]
    fn only_one_refresh_at_a_time() {
        let store = store(None);
        assert!(store.begin_refresh());
        assert!(!store.begin_refresh());
        assert!(store.snapshot().refreshing);
        store.fail_refresh("boom".into());
        assert!(!store.snapshot().refreshing);
        assert_eq!(store.snapshot().problems, vec!["boom"]);
    }
}
