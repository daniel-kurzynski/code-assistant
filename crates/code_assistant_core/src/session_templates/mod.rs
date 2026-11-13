//! Session templates: a pre-configured session with a first message, and
//! optionally the rules for when the app should suggest one.
//!
//! A template is a Markdown file in `<config_dir>/session-templates/`. Its
//! YAML front matter sets up the session (model, effort, permission tier,
//! sandbox, MCP servers, built-in tool groups, a worktree) and holds the
//! `suggest:` rules; its body is the first user message. Placeholders such
//! as `{{issue.url}}` are filled from the issue or pull request the session
//! is started for (see [`placeholders`]).
//!
//! A session started from a template is an ordinary session: nothing runs
//! until the user sends the message (unless the template says `send: true`).
//! Suggestions are computed by [`crate::suggestions`]; nothing is bundled.
//!
//! See `docs/suggested-sessions-plan.md`.

pub mod placeholders;

use crate::session::SessionConfig;
use anyhow::{Context, Result, bail};
use llm::capabilities::Effort;
use sandbox::SandboxPolicy;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tools_core::PermissionTier;

/// The folder below the config dir templates are read from.
pub const TEMPLATES_DIR: &str = "session-templates";

/// What a template's session works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Issue,
    PullRequest,
    #[default]
    None,
}

impl SubjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::PullRequest => "pull_request",
            Self::None => "none",
        }
    }
}

/// The sandbox a template asks for, by its short name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxChoice {
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

impl SandboxChoice {
    pub fn policy(self) -> SandboxPolicy {
        match self {
            Self::ReadOnly => SandboxPolicy::new_read_only(),
            Self::WorkspaceWrite => SandboxPolicy::new_workspace_write(),
            Self::FullAccess => SandboxPolicy::DangerFullAccess,
        }
    }
}

/// Built-in tool groups switched on or off, by group id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinToolChoice {
    #[serde(default)]
    pub on: Vec<String>,
    #[serde(default)]
    pub off: Vec<String>,
}

/// A worktree to create for the session in the subject's repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeSpec {
    /// Branch name; placeholders allowed. An existing worktree of the branch
    /// is reused.
    pub branch: String,
    /// Where a new branch starts; default: the repository's current HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// Which list a rule searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Issues,
    PullRequests,
}

/// The check state a pull request rule asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksFilter {
    Failing,
    Passing,
    Pending,
}

/// A board column filter: the item sits on project `owner/number` with the
/// single-select `field` (default "Status") set to `column`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardFilter {
    /// `owner/number`, e.g. `acme/12`.
    pub project: String,
    pub column: String,
    #[serde(default = "default_board_field")]
    pub field: String,
}

fn default_board_field() -> String {
    "Status".to_string()
}

impl BoardFilter {
    /// `(owner, number)` from `owner/number`.
    pub fn project_ref(&self) -> Result<(String, u64)> {
        let (owner, number) = self
            .project
            .rsplit_once('/')
            .with_context(|| format!("board project {:?} is not `owner/number`", self.project))?;
        let number = number
            .parse()
            .with_context(|| format!("board project {:?} is not `owner/number`", self.project))?;
        Ok((owner.to_string(), number))
    }
}

/// When to suggest a template: one search with filters. Every filter is
/// optional and all of them must hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuggestRule {
    pub when: RuleKind,
    /// Limit to some of the project's repositories (`owner/name`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    /// `me`, a login, `none` or `any`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// `me` or a login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// `me` or a login (pull requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_requested: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels_all: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels_any: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels_none: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<BoardFilter>,
    /// Pull requests only; default `false` (drafts are not suggested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<ChecksFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_within_days: Option<u32>,
}

/// The front matter of a template file. Unknown keys are errors, so a typo
/// does not silently drop a setting.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontMatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    subject: SubjectKind,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    projects: Option<Vec<String>>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<Effort>,
    #[serde(default)]
    permission_tier: Option<PermissionTier>,
    #[serde(default)]
    sandbox: Option<SandboxChoice>,
    #[serde(default)]
    mcp_servers: Option<Vec<String>>,
    #[serde(default)]
    builtin_tools: Option<BuiltinToolChoice>,
    #[serde(default)]
    worktree: Option<WorktreeSpec>,
    #[serde(default)]
    send: bool,
    #[serde(default)]
    suggest: Vec<SuggestRule>,
}

/// A loaded, validated template.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTemplate {
    pub name: String,
    pub description: String,
    pub subject: SubjectKind,
    /// Session title; placeholders allowed. Default: none (named as usual).
    pub title: Option<String>,
    /// Projects (by name) the template is offered in; `None` = all.
    pub projects: Option<Vec<String>>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub permission_tier: Option<PermissionTier>,
    pub sandbox: Option<SandboxChoice>,
    /// Exactly these MCP servers on; `None` keeps the project's selection.
    pub mcp_servers: Option<Vec<String>>,
    pub builtin_tools: BuiltinToolChoice,
    pub worktree: Option<WorktreeSpec>,
    /// Send the first message right away instead of putting it in the input.
    pub send: bool,
    pub suggest: Vec<SuggestRule>,
    /// The first user message; placeholders allowed.
    pub message: String,
    pub path: PathBuf,
}

impl SessionTemplate {
    /// Whether the template is offered in `project`.
    pub fn applies_to(&self, project: &str) -> bool {
        self.projects
            .as_ref()
            .is_none_or(|projects| projects.iter().any(|p| p == project))
    }
}

/// Parse and validate one template file's content. `fallback_name` (the
/// file stem) names a template without `name:`.
pub fn parse_template(content: &str, fallback_name: &str, path: &Path) -> Result<SessionTemplate> {
    let (front, body) = crate::skills::manifest::split_frontmatter(content)
        .context("a template must start with a YAML front matter block between `---` lines")?;
    let front: FrontMatter = if front.trim().is_empty() {
        FrontMatter::default()
    } else {
        serde_yaml_ng::from_str(&front).context("invalid front matter")?
    };
    let name = front
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| fallback_name.to_string());
    let message = body.trim().to_string();

    let kind = front.subject;
    placeholders::validate(&message, kind).context("in the message")?;
    if let Some(title) = &front.title {
        placeholders::validate(title, kind).context("in `title`")?;
    }
    if let Some(worktree) = &front.worktree {
        placeholders::validate(&worktree.branch, kind).context("in `worktree.branch`")?;
        if kind == SubjectKind::None {
            bail!("`worktree` needs a subject (issue or pull_request) to find the repository");
        }
    }
    if front.send && message.is_empty() {
        bail!("`send: true` with an empty message");
    }
    for (i, rule) in front.suggest.iter().enumerate() {
        let fits = matches!(
            (rule.when, kind),
            (RuleKind::Issues, SubjectKind::Issue)
                | (RuleKind::PullRequests, SubjectKind::PullRequest)
        );
        if !fits {
            bail!(
                "suggest rule {}: `when: {}` does not fit `subject: {}`",
                i + 1,
                match rule.when {
                    RuleKind::Issues => "issues",
                    RuleKind::PullRequests => "pull_requests",
                },
                kind.as_str()
            );
        }
        if rule.when == RuleKind::Issues
            && (rule.review_requested.is_some() || rule.checks.is_some() || rule.draft.is_some())
        {
            bail!(
                "suggest rule {}: `review_requested`, `checks` and `draft` apply to pull requests",
                i + 1
            );
        }
        if let Some(board) = &rule.board {
            board
                .project_ref()
                .with_context(|| format!("suggest rule {}", i + 1))?;
        }
    }
    let builtin_tools = front.builtin_tools.unwrap_or_default();
    for id in builtin_tools.on.iter().chain(&builtin_tools.off) {
        if !crate::tools::builtin_selection::GROUPS
            .iter()
            .any(|group| group.id == id)
        {
            let ids: Vec<_> = crate::tools::builtin_selection::GROUPS
                .iter()
                .map(|group| group.id)
                .collect();
            bail!(
                "unknown built-in tool group `{id}`; known: {}",
                ids.join(", ")
            );
        }
    }

    Ok(SessionTemplate {
        name,
        description: front.description.unwrap_or_default().trim().to_string(),
        subject: kind,
        title: front.title,
        projects: front.projects,
        model: front.model,
        effort: front.effort,
        permission_tier: front.permission_tier,
        sandbox: front.sandbox,
        mcp_servers: front.mcp_servers,
        builtin_tools,
        worktree: front.worktree,
        send: front.send,
        suggest: front.suggest,
        message,
        path: path.to_path_buf(),
    })
}

/// A template file that could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateError {
    pub path: PathBuf,
    pub message: String,
}

/// Every template in a folder, plus the files that failed.
#[derive(Debug, Clone, Default)]
pub struct LoadedTemplates {
    pub templates: Vec<SessionTemplate>,
    pub errors: Vec<TemplateError>,
}

impl LoadedTemplates {
    pub fn get(&self, name: &str) -> Option<&SessionTemplate> {
        self.templates.iter().find(|t| t.name == name)
    }
}

/// The folder templates are read from.
pub fn templates_dir() -> PathBuf {
    crate::config_dir::config_dir().join(TEMPLATES_DIR)
}

/// Load `<config_dir>/session-templates/*.md`.
pub fn load_templates() -> LoadedTemplates {
    load_templates_from(&templates_dir())
}

/// Load every `*.md` in `dir`, sorted by file name. A missing folder has no
/// templates; a second template with a name already taken is an error.
pub fn load_templates_from(dir: &Path) -> LoadedTemplates {
    let mut loaded = LoadedTemplates::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return loaded;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
        .collect();
    paths.sort();
    for path in paths {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let result = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))
            .and_then(|content| parse_template(&content, &stem, &path));
        match result {
            Ok(template) if loaded.get(&template.name).is_some() => {
                loaded.errors.push(TemplateError {
                    message: format!("another template is already named `{}`", template.name),
                    path,
                })
            }
            Ok(template) => loaded.templates.push(template),
            Err(e) => loaded.errors.push(TemplateError {
                path,
                message: format!("{e:#}"),
            }),
        }
    }
    loaded
}

/// What could not be applied as asked; the session starts anyway.
pub type Notices = Vec<String>;

/// Apply a template's session setup to a freshly seeded config (see
/// [`crate::session::SessionManager::new_session_config`]): permission tier,
/// sandbox, MCP servers and built-in tool groups. Unknown MCP servers are
/// reported, not fatal.
pub fn apply_to_config(template: &SessionTemplate, config: &mut SessionConfig) -> Notices {
    let mut notices = Vec::new();
    if let Some(tier) = template.permission_tier {
        config.permission_tier = tier;
    }
    if let Some(sandbox) = template.sandbox {
        config.sandbox_policy = sandbox.policy();
    }
    if let Some(wanted) = &template.mcp_servers {
        let known: Vec<String> = config
            .enabled_mcp_servers
            .iter()
            .chain(&config.disabled_mcp_servers)
            .cloned()
            .collect();
        for name in wanted {
            if !known.contains(name) {
                notices.push(format!("MCP server `{name}` is not configured"));
            }
        }
        for name in known {
            let on = wanted.contains(&name);
            crate::tools::mcp_selection::switch_server(config, &name, on);
        }
    }
    for id in &template.builtin_tools.on {
        crate::tools::builtin_selection::switch_group(config, id, true);
    }
    for id in &template.builtin_tools.off {
        crate::tools::builtin_selection::switch_group(config, id, false);
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARCHITECT: &str = r#"---
name: architect
description: Plan a ticket
subject: issue
title: "Plan #{{issue.number}} {{issue.title}}"
model: anthropic/claude-opus-5-5
effort: high
permission_tier: write-tools
sandbox: read-only
mcp_servers: [github]
builtin_tools: { on: [web], off: [browser] }
suggest:
  - when: issues
    assignee: me
    labels_all: [agent]
    board: { project: acme/12, column: Refinement }
---
Write an implementation plan for {{issue.url}}.
"#;

    fn parse(content: &str) -> Result<SessionTemplate> {
        parse_template(content, "file-stem", Path::new("/t/x.md"))
    }

    #[test]
    fn a_full_template_parses() {
        let t = parse(ARCHITECT).unwrap();
        assert_eq!(t.name, "architect");
        assert_eq!(t.subject, SubjectKind::Issue);
        assert_eq!(t.effort, Some(Effort::High));
        assert_eq!(t.permission_tier, Some(PermissionTier::WriteTools));
        assert_eq!(t.sandbox, Some(SandboxChoice::ReadOnly));
        assert_eq!(t.message, "Write an implementation plan for {{issue.url}}.");
        let rule = &t.suggest[0];
        assert_eq!(rule.when, RuleKind::Issues);
        assert_eq!(rule.assignee.as_deref(), Some("me"));
        let board = rule.board.as_ref().unwrap();
        assert_eq!(board.field, "Status");
        assert_eq!(board.project_ref().unwrap(), ("acme".into(), 12));
    }

    #[test]
    fn the_file_stem_names_a_template_without_a_name() {
        let t = parse("---\n---\nHello").unwrap();
        assert_eq!(t.name, "file-stem");
        assert_eq!(t.subject, SubjectKind::None);
    }

    #[test]
    fn mistakes_are_reported() {
        let cases = [
            ("no front matter", "Hello"),
            ("typo", "---\nmodle: x\n---\nHi"),
            ("placeholder", "---\nsubject: issue\n---\n{{pr.number}}"),
            (
                "rule kind",
                "---\nsubject: issue\nsuggest:\n  - when: pull_requests\n---\nHi",
            ),
            (
                "pr filter on issues",
                "---\nsubject: issue\nsuggest:\n  - when: issues\n    checks: failing\n---\nHi",
            ),
            (
                "board ref",
                "---\nsubject: issue\nsuggest:\n  - when: issues\n    board: { project: acme, column: X }\n---\nHi",
            ),
            (
                "tool group",
                "---\nbuiltin_tools: { on: [lasers] }\n---\nHi",
            ),
            (
                "worktree without subject",
                "---\nworktree: { branch: x }\n---\nHi",
            ),
            ("send nothing", "---\nsend: true\n---\n"),
        ];
        for (what, content) in cases {
            assert!(parse(content).is_err(), "{what} should fail");
        }
    }

    #[test]
    fn a_folder_loads_sorted_and_reports_bad_files_and_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.md"), ARCHITECT).unwrap();
        std::fs::write(dir.path().join("a.md"), "---\nname: architect\n---\nOther").unwrap();
        std::fs::write(dir.path().join("c.md"), "no front matter").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        let loaded = load_templates_from(dir.path());
        assert_eq!(loaded.templates.len(), 1);
        assert_eq!(loaded.templates[0].message, "Other");
        assert_eq!(loaded.errors.len(), 2);
        assert!(loaded.errors[0].message.contains("already named"));
        assert!(
            load_templates_from(&dir.path().join("missing"))
                .templates
                .is_empty()
        );
    }

    #[test]
    fn the_setup_is_applied_to_a_seeded_config() {
        let t = parse(ARCHITECT).unwrap();
        let mut config = SessionConfig {
            enabled_mcp_servers: vec!["slack".into()],
            disabled_mcp_servers: vec!["github".into()],
            ..Default::default()
        };
        let notices = apply_to_config(&t, &mut config);
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(config.permission_tier, PermissionTier::WriteTools);
        assert_eq!(config.sandbox_policy, SandboxPolicy::ReadOnly);
        assert_eq!(config.enabled_mcp_servers, vec!["github"]);
        assert_eq!(config.disabled_mcp_servers, vec!["slack"]);
        assert!(config.enabled_builtin_tools.contains(&"web".to_string()));
        assert!(
            config
                .disabled_builtin_tools
                .contains(&"browser".to_string())
        );

        let mut bare = SessionConfig::default();
        let notices = apply_to_config(&t, &mut bare);
        assert_eq!(notices, vec!["MCP server `github` is not configured"]);
    }

    #[test]
    fn project_limits() {
        let mut t = parse(ARCHITECT).unwrap();
        assert!(t.applies_to("shop"));
        t.projects = Some(vec!["pal".into()]);
        assert!(!t.applies_to("shop"));
        assert!(t.applies_to("pal"));
    }
}
