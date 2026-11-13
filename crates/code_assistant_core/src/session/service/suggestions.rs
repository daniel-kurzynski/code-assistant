//! Suggested sessions and session templates through the service (see
//! [`crate::suggestions`], [`crate::session_templates`]).

use std::collections::{BTreeMap, HashSet};

use super::*;
use crate::session_templates::{LoadedTemplates, SessionTemplate, SubjectKind, placeholders};
use crate::suggestions::compute::{ProjectRepos, compute, discover_project_repos};
use crate::suggestions::source::GitHubSource;
use crate::suggestions::subject::SubjectUrl;
use crate::suggestions::{Dismissal, SuggestionEnv, SuggestionsSnapshot};

/// Start a session from a template, optionally for an issue or pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFromTemplate {
    pub project: String,
    pub template: String,
    /// The issue or pull request URL, for templates with a subject.
    pub subject_url: Option<String>,
}

/// A session started from a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedSession {
    pub session_id: String,
    /// The rendered first message: for the input, unless `sent`.
    pub first_message: String,
    /// The template sent the message itself (`send: true`).
    pub sent: bool,
    /// What could not be applied as asked; the session started anyway.
    pub notices: Vec<String>,
}

impl SessionService {
    /// Replace where suggestions and templates come from (tests, embedders).
    pub fn set_suggestion_env(&self, env: SuggestionEnv) {
        self.suggestions.set_env(env);
    }

    /// The suggestions as they stand, without asking GitHub.
    pub fn suggestions(&self) -> SuggestionsSnapshot {
        self.suggestions.snapshot()
    }

    fn publish_suggestions(&self) -> SuggestionsSnapshot {
        let snapshot = self.suggestions.snapshot();
        self.events.publish_app(UiEvent::SuggestionsChanged {
            snapshot: snapshot.clone(),
        });
        snapshot
    }

    /// The templates in the templates folder, and the files that failed.
    pub async fn list_session_templates(&self) -> Result<LoadedTemplates> {
        let env = self.suggestions.env();
        self.call_io(move |_| async move {
            let dir = env.templates_dir.clone();
            tokio::task::spawn_blocking(move || crate::session_templates::load_templates_from(&dir))
                .await
                .context("template loading was aborted")
        })
        .await
    }

    /// Ask GitHub again for every template's rules. A refresh already
    /// running answers with the current state. Progress and the result are
    /// published as [`UiEvent::SuggestionsChanged`].
    pub async fn refresh_suggestions(&self) -> Result<SuggestionsSnapshot> {
        if !self.suggestions.begin_refresh() {
            return Ok(self.suggestions.snapshot());
        }
        self.publish_suggestions();
        let env = self.suggestions.env();
        let result = self
            .call_io(move |ctx| async move {
                let (templates, projects) = {
                    let env = env.clone();
                    tokio::task::spawn_blocking(move || {
                        let templates =
                            crate::session_templates::load_templates_from(&env.templates_dir);
                        let projects: Vec<ProjectRepos> = (env.projects)()
                            .iter()
                            .map(|(name, root)| discover_project_repos(name, root))
                            .collect();
                        (templates, projects)
                    })
                    .await
                    .context("project discovery was aborted")?
                };
                let mut problems: Vec<String> = templates
                    .errors
                    .iter()
                    .map(|e| format!("{}: {}", e.path.display(), e.message))
                    .collect();
                let wants_github = templates.templates.iter().any(|t| !t.suggest.is_empty());
                let mut candidates = Vec::new();
                if wants_github {
                    let source = (env.source)(ctx.pull_requests.client());
                    let computed = compute(
                        &templates.templates,
                        &projects,
                        source.as_ref(),
                        chrono::Utc::now(),
                    )
                    .await;
                    candidates = computed.candidates;
                    problems.extend(computed.problems);
                }
                let live_sessions: HashSet<String> = {
                    let manager = ctx.manager.lock().await;
                    manager
                        .list_all_sessions()?
                        .into_iter()
                        .map(|meta| meta.id)
                        .collect()
                };
                Ok((candidates, problems, live_sessions))
            })
            .await;
        let previous = self.suggestions.snapshot().problems;
        match result {
            Ok((candidates, problems, live_sessions)) => {
                self.suggestions
                    .finish_refresh(candidates, problems, Some(live_sessions));
            }
            Err(e) => self.suggestions.fail_refresh(format!("{e:#}")),
        }
        let snapshot = self.publish_suggestions();
        // The list shows problems only while it has suggestions; the log
        // always has them, once per change.
        if snapshot.problems != previous {
            for problem in &snapshot.problems {
                warn!("Suggested sessions: {problem}");
            }
        }
        Ok(snapshot)
    }

    /// Put a suggestion away for now (until its subject changes) or for good.
    pub fn dismiss_suggestion(&self, id: &str, how: Dismissal) -> SuggestionsSnapshot {
        self.suggestions.dismiss(id, how);
        self.publish_suggestions()
    }

    /// The user had the list open while these were in it.
    pub fn mark_suggestions_seen(&self, ids: &[String]) -> SuggestionsSnapshot {
        if self.suggestions.mark_seen(ids) {
            self.publish_suggestions()
        } else {
            self.suggestions.snapshot()
        }
    }

    /// Start the session a suggestion offers. It leaves the list for as long
    /// as the session exists.
    pub async fn start_suggestion(&self, id: String) -> Result<StartedSession> {
        let suggestion = self
            .suggestions
            .candidate(&id)
            .with_context(|| format!("no suggestion {id}"))?;
        let started = self
            .start_from_template(StartFromTemplate {
                project: suggestion.project,
                template: suggestion.template,
                subject_url: Some(suggestion.subject.url),
            })
            .await?;
        self.suggestions.mark_taken(&id, &started.session_id);
        self.publish_suggestions();
        Ok(started)
    }

    /// Create a session from a template: the template's setup applied to a
    /// new session of the project, its title and first message rendered for
    /// the subject. The message goes into the input (returned as
    /// `first_message`) unless the template sends it.
    pub async fn start_from_template(&self, request: StartFromTemplate) -> Result<StartedSession> {
        let env = self.suggestions.env();
        let prepared = self
            .call_io(move |ctx| async move {
                // Someone waits for this one: interactive priority.
                let client = ctx
                    .pull_requests
                    .client()
                    .with_priority(github::Priority::Interactive);
                prepare_start(&env, client, request).await
            })
            .await?;
        let Prepared {
            template,
            project,
            project_root,
            title,
            message,
            worktree,
            mut notices,
        } = prepared;
        let send = template.send;

        let session_id = self
            .call(move |ctx| async move {
                let mut manager = ctx.manager.lock().await;
                let root = project_root.or_else(|| manager.resolve_project_path(&project));
                let Some(root) = root else {
                    bail!("unknown project `{project}`");
                };
                let mut config = manager.new_session_config(Some(root));
                config.initial_project = project;
                notices.extend(crate::session_templates::apply_to_config(
                    &template,
                    &mut config,
                ));
                if let Some((path, branch)) = worktree {
                    config.worktree_path = Some(path);
                    config.branch = Some(branch);
                }
                let model = match &template.model {
                    Some(wanted) => match ConfigurationSystem::load()
                        .ok()
                        .and_then(|system| system.resolve_key(wanted))
                    {
                        Some(key) => key,
                        None => {
                            notices.push(format!(
                                "model `{wanted}` is not configured; using the default model"
                            ));
                            manager.default_model_name()
                        }
                    },
                    None => manager.default_model_name(),
                };
                let model_config = SessionModelConfig::new(model).with_effort(template.effort);
                let session_id =
                    manager.create_session_with_config(title, Some(config), Some(model_config))?;
                Ok((session_id, notices))
            })
            .await;
        let (session_id, notices) = session_id?;

        let mut started = StartedSession {
            session_id: session_id.clone(),
            first_message: message.clone(),
            sent: false,
            notices,
        };
        if send {
            self.send_user_message(session_id, message, Vec::new(), None)
                .await?;
            started.sent = true;
        }
        Ok(started)
    }
}

/// Everything resolved before the session is created.
struct Prepared {
    template: SessionTemplate,
    project: String,
    project_root: Option<PathBuf>,
    title: Option<String>,
    message: String,
    /// `(path, branch)` of the worktree created for the session.
    worktree: Option<(PathBuf, String)>,
    notices: Vec<String>,
}

async fn prepare_start(
    env: &SuggestionEnv,
    client: github::Client,
    request: StartFromTemplate,
) -> Result<Prepared> {
    let templates = {
        let dir = env.templates_dir.clone();
        tokio::task::spawn_blocking(move || crate::session_templates::load_templates_from(&dir))
            .await
            .context("template loading was aborted")?
    };
    let template = templates
        .get(&request.template)
        .cloned()
        .with_context(|| format!("no session template named `{}`", request.template))?;
    let project_root = (env.projects)()
        .into_iter()
        .find(|(name, _)| *name == request.project)
        .map(|(_, root)| root);

    let mut values: BTreeMap<String, String> = BTreeMap::new();
    values.insert("project.name".into(), request.project.clone());
    let mut worktree = None;

    match (template.subject, request.subject_url.as_deref()) {
        (SubjectKind::None, None) => {}
        (SubjectKind::None, Some(_)) => {
            bail!(
                "template `{}` has no subject; it takes no URL",
                template.name
            )
        }
        (_, None) => bail!(
            "template `{}` needs the URL of {}",
            template.name,
            match template.subject {
                SubjectKind::Issue => "an issue",
                _ => "a pull request",
            }
        ),
        (kind, Some(url)) => {
            let subject = SubjectUrl::parse(url)
                .with_context(|| format!("not an issue or pull request URL: {url}"))?;
            if subject.kind != kind {
                bail!(
                    "template `{}` is for {}, {url} is not one",
                    template.name,
                    match kind {
                        SubjectKind::Issue => "issues",
                        _ => "pull requests",
                    }
                );
            }
            let source: Arc<dyn GitHubSource> = (env.source)(client);
            let details = source.issue(&subject.repo, subject.number).await?;
            let login = source.login(&subject.repo.host).await?;
            let prefix = match kind {
                SubjectKind::Issue => "issue",
                _ => "pr",
            };
            let summary = &details.summary;
            values.insert("me.login".into(), login);
            for (key, value) in [
                ("number", summary.number.to_string()),
                ("title", summary.title.clone()),
                ("url", summary.url.clone()),
                ("repo", summary.repo.full_name()),
                ("author", summary.author.clone().unwrap_or_default()),
                ("labels", summary.labels.join(", ")),
                ("slug", placeholders::slug(&summary.title)),
                ("body", details.body.clone()),
            ] {
                values.insert(format!("{prefix}.{key}"), value);
            }
            if kind == SubjectKind::PullRequest {
                values.insert(
                    "pr.head".into(),
                    summary.head_branch.clone().unwrap_or_default(),
                );
                values.insert(
                    "pr.base".into(),
                    summary.base_branch.clone().unwrap_or_default(),
                );
            }
            if let Some(spec) = &template.worktree {
                let branch =
                    placeholders::branch_safe(&placeholders::render(&spec.branch, &values)?);
                if branch.is_empty() {
                    bail!("the worktree branch of `{}` renders empty", template.name);
                }
                let root = project_root
                    .clone()
                    .with_context(|| format!("unknown project `{}`", request.project))?;
                let path = ensure_worktree(
                    &request.project,
                    root,
                    &subject.repo,
                    &branch,
                    spec.base.as_deref(),
                )
                .await?;
                worktree = Some((path, branch));
            }
        }
    }

    let message = placeholders::render(&template.message, &values)?;
    let title = template
        .title
        .as_deref()
        .map(|title| placeholders::render(title, &values))
        .transpose()?
        .map(|title| title.trim().to_string())
        .filter(|title| !title.is_empty());
    Ok(Prepared {
        project: request.project,
        project_root,
        title,
        message,
        worktree,
        notices: Vec::new(),
        template,
    })
}

/// The worktree of `branch` in the project's checkout of `repo`: reused when
/// it exists, else created (starting at `base`, default HEAD).
async fn ensure_worktree(
    project: &str,
    root: PathBuf,
    repo: &github::RepoRef,
    branch: &str,
    base: Option<&str>,
) -> Result<PathBuf> {
    let project_name = project.to_string();
    let repos = tokio::task::spawn_blocking(move || discover_project_repos(&project_name, &root))
        .await
        .context("repository discovery was aborted")?;
    let local = repos
        .repos
        .iter()
        .find(|local| {
            local.repo.host == repo.host
                && local.repo.owner.eq_ignore_ascii_case(&repo.owner)
                && local.repo.name.eq_ignore_ascii_case(&repo.name)
        })
        .with_context(|| {
            format!(
                "project `{project}` has no checkout of {} (no checkout with a GitHub remote for it)",
                repo.full_name()
            )
        })?;
    let git_repo = git::GitRepository::open(&local.path).context("cannot open the repository")?;
    if let Ok(Some(existing)) = git::worktree::find_worktree_for_branch(&git_repo, branch).await {
        return Ok(existing.path);
    }
    let path = git::worktree::suggest_worktree_path(git_repo.workdir(), branch);
    git::worktree::create_worktree(&git_repo.git, git_repo.workdir(), &path, branch, base)
        .await
        .context("cannot create the worktree")
}

#[cfg(test)]
mod tests {
    use super::super::tests::{scripted_turn, test_service_with_llm, test_service_with_manager};
    use super::*;
    use crate::suggestions::query::search_query;
    use crate::suggestions::source::FakeGitHub;
    use github::RepoRef;
    use github::model::{IssueDetail, IssueSummary};
    use std::path::Path;
    use tools_core::PermissionTier;

    const ARCHITECT: &str = r#"---
name: architect
description: Plan a ticket
subject: issue
title: "Plan #{{issue.number}} {{issue.title}}"
permission_tier: write-tools
suggest:
  - when: issues
    assignee: me
    labels_all: [agent]
---
Plan {{issue.url}} in {{issue.repo}} for {{me.login}} ({{project.name}})."#;

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

    /// A project folder holding one repository whose origin is acme/shop.
    fn project_repo(root: &Path) -> PathBuf {
        let repo = root.join("shop-project").join("shop");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:acme/shop.git"],
        );
        repo.parent().unwrap().to_path_buf()
    }

    fn issue(number: u64) -> IssueSummary {
        IssueSummary {
            repo: RepoRef::new("github.com", "acme", "shop"),
            number,
            title: "Coupons at checkout".into(),
            url: format!("https://github.com/acme/shop/issues/{number}"),
            author: Some("maria".into()),
            labels: vec!["agent".into()],
            assignees: vec!["dk".into()],
            is_pull_request: false,
            is_draft: false,
            head_branch: None,
            base_branch: None,
            node_id: format!("I_{number}"),
            updated_at: chrono::Utc::now(),
        }
    }

    fn env(dir: &Path, project_root: PathBuf, fake: Arc<FakeGitHub>) -> SuggestionEnv {
        SuggestionEnv {
            templates_dir: dir.join("templates"),
            state_path: None,
            projects: Arc::new(move || vec![("shop".to_string(), project_root.clone())]),
            source: Arc::new(move |_| fake.clone() as Arc<dyn GitHubSource>),
        }
    }

    fn write_template(dir: &Path, name: &str, content: &str) {
        std::fs::create_dir_all(dir.join("templates")).unwrap();
        std::fs::write(dir.join("templates").join(format!("{name}.md")), content).unwrap();
    }

    fn fake_with_issue(template: &str) -> FakeGitHub {
        let template =
            crate::session_templates::parse_template(template, "t", Path::new("/t")).unwrap();
        let mut fake = FakeGitHub {
            login: "dk".into(),
            ..Default::default()
        };
        let query = search_query(
            &template.suggest[0],
            &[RepoRef::new("github.com", "acme", "shop")],
            chrono::Utc::now(),
        );
        fake.searches
            .lock()
            .unwrap()
            .insert(("github.com".into(), query), vec![issue(42)]);
        fake.issues.push(IssueDetail {
            summary: issue(42),
            body: "Add coupons".into(),
        });
        fake
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_suggestion_becomes_a_configured_session_with_its_message() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = project_repo(tmp.path());
        write_template(tmp.path(), "architect", ARCHITECT);
        let fake = Arc::new(fake_with_issue(ARCHITECT));
        let (service, manager) = test_service_with_manager(&tmp.path().join("sessions"));
        service.set_suggestion_env(env(tmp.path(), project_root.clone(), fake));
        let mut events = service.subscribe();

        let snapshot = service.refresh_suggestions().await.unwrap();
        assert!(snapshot.problems.is_empty(), "{:?}", snapshot.problems);
        assert_eq!(snapshot.suggestions.len(), 1);
        let suggestion = &snapshot.suggestions[0];
        assert_eq!(suggestion.project, "shop");
        assert_eq!(suggestion.reason, "assigned to you · agent");
        assert!(snapshot.has_unseen());
        // A refresh announces itself and its result.
        let mut announced = 0;
        while let Ok(Ok(event)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), events.recv()).await
        {
            if matches!(
                event.payload,
                crate::session::event_stream::EventPayload::Ui(UiEvent::SuggestionsChanged { .. })
            ) {
                announced += 1;
            }
        }
        assert_eq!(announced, 2);

        let started = service
            .start_suggestion(suggestion.id.clone())
            .await
            .unwrap();
        assert!(!started.sent);
        assert_eq!(
            started.first_message,
            "Plan https://github.com/acme/shop/issues/42 in acme/shop for dk (shop)."
        );
        assert!(service.suggestions().suggestions.is_empty(), "taken");

        let manager = manager.lock().await;
        let session = &manager.get_session(&started.session_id).unwrap().session;
        assert_eq!(session.name, "Plan #42 Coupons at checkout");
        assert_eq!(session.config.permission_tier, PermissionTier::WriteTools);
        assert_eq!(session.config.initial_project, "shop");
        assert_eq!(
            session.config.init_path.as_deref(),
            Some(project_root.as_path())
        );
        assert_eq!(session.message_count(), 0, "nothing sent");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_worktree_template_checks_out_a_branch_in_the_subjects_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = project_repo(tmp.path());
        let implementer = "---\nname: implementer\nsubject: issue\nworktree: { branch: \"{{me.login}}/{{issue.number}}-{{issue.slug}}\" }\n---\nImplement {{issue.url}}";
        write_template(tmp.path(), "implementer", implementer);
        let fake = Arc::new(fake_with_issue(ARCHITECT));
        let (service, manager) = test_service_with_manager(&tmp.path().join("sessions"));
        service.set_suggestion_env(env(tmp.path(), project_root, fake));

        let started = service
            .start_from_template(StartFromTemplate {
                project: "shop".into(),
                template: "implementer".into(),
                subject_url: Some("https://github.com/acme/shop/issues/42".into()),
            })
            .await
            .unwrap();
        let manager = manager.lock().await;
        let config = &manager
            .get_session(&started.session_id)
            .unwrap()
            .session
            .config;
        assert_eq!(config.branch.as_deref(), Some("dk/42-coupons-at-checkout"));
        let worktree = config.worktree_path.clone().unwrap();
        assert!(worktree.join(".git").exists(), "{}", worktree.display());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_subjects_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = project_repo(tmp.path());
        write_template(tmp.path(), "architect", ARCHITECT);
        write_template(
            tmp.path(),
            "plain",
            "---\nname: plain\n---\nHello {{project.name}}",
        );
        let (service, _) = test_service_with_manager(&tmp.path().join("sessions"));
        let fake = fake_with_issue(ARCHITECT);
        service.set_suggestion_env(env(tmp.path(), project_root, Arc::new(fake)));
        let start = |template: &str, url: Option<&str>| {
            service.start_from_template(StartFromTemplate {
                project: "shop".into(),
                template: template.into(),
                subject_url: url.map(String::from),
            })
        };
        assert!(start("architect", None).await.is_err());
        assert!(
            start("architect", Some("https://github.com/acme/shop/pull/7"))
                .await
                .is_err()
        );
        assert!(
            start("plain", Some("https://github.com/acme/shop/issues/42"))
                .await
                .is_err()
        );
        assert!(start("missing", None).await.is_err());
        let plain = start("plain", None).await.unwrap();
        assert_eq!(plain.first_message, "Hello shop");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_template_that_sends_starts_the_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = project_repo(tmp.path());
        write_template(tmp.path(), "go", "---\nname: go\nsend: true\n---\nGo");
        let (service, manager) =
            test_service_with_llm(&tmp.path().join("sessions"), scripted_turn("done"));
        service.set_suggestion_env(env(
            tmp.path(),
            project_root,
            Arc::new(FakeGitHub::default()),
        ));
        let started = service
            .start_from_template(StartFromTemplate {
                project: "shop".into(),
                template: "go".into(),
                subject_url: None,
            })
            .await
            .unwrap();
        assert!(started.sent);
        let manager = manager.lock().await;
        let session = &manager.get_session(&started.session_id).unwrap().session;
        assert!(session.message_count() >= 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn template_errors_and_missing_tokens_show_as_problems() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = project_repo(tmp.path());
        write_template(tmp.path(), "architect", ARCHITECT);
        write_template(tmp.path(), "broken", "---\nmodle: x\n---\nHi");
        let mut fake = FakeGitHub::default();
        fake.failing_hosts
            .insert("github.com".into(), "no GitHub token for github.com".into());
        let (service, _) = test_service_with_manager(&tmp.path().join("sessions"));
        service.set_suggestion_env(env(tmp.path(), project_root, Arc::new(fake)));
        let snapshot = service.refresh_suggestions().await.unwrap();
        assert!(snapshot.suggestions.is_empty());
        assert_eq!(snapshot.problems.len(), 2, "{:?}", snapshot.problems);
        assert!(snapshot.problems[0].contains("broken.md"));
        assert!(snapshot.problems[1].contains("no GitHub token"));
    }
}
