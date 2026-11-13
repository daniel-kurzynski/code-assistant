# Suggested sessions

Status: prototype (2026-11-13). To set it up step by step, read
[suggested-sessions-setup.md](suggested-sessions-setup.md); this page is the
reference. The design and the steps still open are in
`docs/suggested-sessions-plan.md`.

The app notices issues and pull requests on GitHub that fit a session
template and offers to start a session for them. The offers sit behind a
light bulb left of the sidebar's "+" button. One click creates an ordinary
session — model, effort, permission tier, sandbox, MCP servers, tool groups
and optionally a worktree set from the template — and puts the template's
first message into the input, ready to edit and send.

Nothing runs by itself: the user sends the message, chats as in any session,
and permissions prompt as the tier says. Nothing is bundled either; without
templates there are no suggestions and no light bulb.

## Session templates

One Markdown file per template in `<config_dir>/session-templates/`
(`~/.config/code-assistant/session-templates/` by default). The YAML front
matter sets up the session and says when to suggest it; the body is the
first user message.

```markdown
---
name: architect
description: Plan a ticket before it is implemented
subject: issue                  # issue | pull_request | none
title: "Plan #{{issue.number}} {{issue.title}}"
projects: [shop]                # optional: only these projects
model: anthropic/claude-opus-5-5
effort: high                    # off | minimal | low | medium | high | xhigh | max
permission_tier: write-tools    # bypass-all | outward-tools | write-tools | all-tools
sandbox: read-only              # read-only | workspace-write | full-access
mcp_servers: [github]           # exactly these on; omit to keep the project's selection
builtin_tools: { on: [web, sub-agents], off: [browser] }
send: false                     # true: send the message right away
suggest:
  - when: issues
    assignee: me
    labels_all: [agent]
    board: { project: acme/12, column: Refinement }
---
Write an implementation plan for {{issue.url}} ({{issue.repo}}).

Read the ticket and its comments first, explore the code, then add the plan
to the ticket as a section "Plan".
```

A template for pull requests, with a worktree:

```markdown
---
name: implementer
subject: issue
permission_tier: outward-tools
sandbox: workspace-write
worktree: { branch: "{{me.login}}/{{issue.number}}-{{issue.slug}}" }
suggest:
  - when: issues
    assignee: me
    board: { project: acme/12, column: Implementation }
---
Implement {{issue.url}} following the plan in the ticket, then open a pull
request.
```

Every key is optional; a missing one leaves the project's default. Unknown
keys are errors, so a typo does not silently drop a setting. The name
defaults to the file name. `worktree` creates (or reuses) a worktree of the
branch in the project's checkout of the subject's repository — also when the
project folder holds several repositories.

### Placeholders

`{{name}}`, replaced as text. Using a name the subject kind does not offer is
an error when the file is loaded.

| Subject | Placeholders |
| --- | --- |
| any | `project.name` |
| `issue` | `me.login`, `issue.number`, `.title`, `.url`, `.repo`, `.author`, `.labels`, `.slug`, `.body` |
| `pull_request` | `me.login`, `pr.number`, `.title`, `.url`, `.repo`, `.author`, `.labels`, `.slug`, `.body`, `.head`, `.base` |

Values are read when the session starts, not when the suggestion was found.
`slug` is the title as lowercase ASCII words joined by dashes (at most 40
characters); a rendered branch name loses what git rejects. Prefer `.url`
over `.body`: the model can read the current ticket with a GitHub MCP server
or `gh`.

### Suggestion rules

`suggest:` is a list; any rule that matches offers the template. All
filters of a rule must hold:

| Filter | Values |
| --- | --- |
| `when` | `issues` (for `subject: issue`) or `pull_requests` (for `subject: pull_request`) |
| `repos` | limit to some of the project's repositories (`owner/name`) |
| `assignee` | `me`, a login, `none`, `any` |
| `author` | `me`, a login |
| `review_requested` | `me`, a login (pull requests) |
| `labels_all`, `labels_any`, `labels_none` | label names |
| `board` | `{ project: owner/number, column: Name, field: Status }` (field optional) |
| `draft` | pull requests; default `false` |
| `checks` | `failing`, `passing`, `pending` (pull requests) |
| `updated_within_days` | number |

Everything but `board` becomes one GitHub search query per rule and server.
The board column is checked afterwards through GraphQL (Projects v2), which
needs the `read:project` scope: `gh auth refresh --scopes read:project`.

## Which repositories, which server

GitHub is read exactly as the pull request view reads it
([pull-request-view.md](pull-request-view.md)): through the same client,
with the same token, endpoints and rate limits.

- **Repositories**: only the projects in `projects.json`. For each, the
  folder's own repository, then the checkouts directly below it (hidden
  folders such as `.worktrees` are skipped). A checkout belongs to the
  GitHub repository of its `origin` remote, else of its first remote that
  names one, so github.com and GitHub Enterprise servers work side by side.
- **Endpoints**: `https://api.github.com` for github.com,
  `https://api.<tenant>.ghe.com` for GHE.com, `https://<host>/api/v3` and
  `https://<host>/api/graphql` for an Enterprise server. The host is taken
  from the remote as written, so a remote using an SSH alias from
  `~/.ssh/config` is not recognised; use the server's real host name.
- **Token**: `GH_TOKEN` / `GITHUB_TOKEN` for github.com and GHE.com;
  `GH_ENTERPRISE_TOKEN` / `GITHUB_ENTERPRISE_TOKEN` only for the server
  `GH_HOST` names; otherwise `gh auth token --hostname <host>`. The app
  stores no token.

## The light bulb

- Shown only while there are suggestions; it lights up (and carries a dot)
  while one has not been seen. Opening the list marks everything in it as
  seen; what was new keeps its dot until the list closes.
- Rows are grouped by project: the template, `#number title`, the repository
  and why the rule matched. The tooltip shows the URL and the template's
  description.
- A click starts the session and opens it with the first message in the
  input. Hovering shows *Not now* (back when the issue or pull request
  changes) and *Never*.
- A started suggestion leaves the list for as long as its session exists.
- The header refreshes on demand; otherwise suggestions refresh at startup
  and every five minutes while the app runs. Problems (no token, a broken
  template, a missing scope) show at the bottom of the list and in the log.

State survives restarts in `<data_dir>/suggestions.json` (seen, dismissed,
taken).

## Code

| Where | What |
| --- | --- |
| `crates/github` (the pull request view's client) | `Client::search_issues`, `issue_or_pull_request`, `board_placements` (`ops/issues.rs`, `graphql/issues.graphql`, `model/issues.rs`) |
| `code_assistant_core::session_templates` | front matter, validation, placeholders, `apply_to_config` |
| `code_assistant_core::suggestions` | `query` (rule → search query, reason), `compute` (rules × projects → candidates, projects through `pull_requests::github_checkouts`), `source` (`GitHubSource`; `LiveGitHub` over the service's shared `github::Client`; `FakeGitHub`), `subject` (issue / pull request URLs), `SuggestionStore`, `run_suggestion_refresher` |
| `SessionService` (`session/service/suggestions.rs`) | `refresh_suggestions`, `suggestions`, `dismiss_suggestion`, `mark_suggestions_seen`, `start_suggestion`, `start_from_template`, `list_session_templates`, `set_suggestion_env` |
| `UiEvent::SuggestionsChanged` | app-scoped snapshot after every change |
| `ui_gpui` | `sidebar/suggestion_list.rs`, the bulb in `sidebar/mod.rs`, `app/suggestions.rs` |

Tests run against `FakeGitHub` and real temporary git repositories
(`session/service/suggestions.rs`); the client operations against the
`github` crate's scripted `FakeTransport` (`crates/github/tests/issues.rs`).
