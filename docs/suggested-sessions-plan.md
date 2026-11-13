# Suggested sessions

Status: proposal (2026-11-13). Nothing here is implemented. This is the first
scope on the way to `docs/autonomous-agents-plan.md` and can stand alone.

## What it is

The app notices work on GitHub that fits a template I have configured and
offers to start a session for it. The offer sits under the project in the
"+" popover. One click creates an ordinary session: model, permission tier,
sandbox, MCP servers and tool groups set from the template, and the first
message written out in the input, ready to edit and send.

It is not an autonomous agent. Nothing runs until I send the message, I can
chat as in any session, and permissions prompt as the tier says.

```text
 GitHub ──gh──▶ sources ──▶ filters ──▶ triage (optional) ──▶ suggestions
                                                                   │ click
                              session template ───────────────────▶ new session,
                              (front matter + first message)        input pre-filled
```

## Names

"Agent" and "automation" promise something that acts by itself; here nothing
does. Proposed vocabulary, matching the code's "session":

| Term | Meaning |
| --- | --- |
| **Session template** | a Markdown file: how the session is set up and what its first message says |
| **Suggestion rule** | the `suggest:` part of a template: when to offer it |
| **Suggestion** | one offer: this template for this issue or pull request |

In the UI: *Templates* in the settings, *Suggested* in the popover. "Agent"
stays free for the later stage, where a template is sent without me and runs
without prompts; the files and rules stay the same.

## Session templates

One file per template, front matter plus body, parsed like `SKILL.md`
(`serde_yaml_ng` is already a dependency). The body is the first user
message.

```markdown
---
name: architect
description: Plan a ticket before it is implemented
subject: issue                         # issue | pull_request | none
title: "Plan #{{issue.number}} {{issue.title}}"

# session setup — every key optional, missing ones use the project's defaults
model: anthropic/claude-opus-5-5
effort: high
permission_tier: write-tools           # bypass-all | outward-tools | write-tools | all-tools
sandbox: read-only                     # read-only | workspace-write | full-access
mcp_servers: [github]                  # exactly these on; omit to keep the project's selection
builtin_tools: { on: [web, sub-agents], off: [browser, terminal] }
worktree: none                         # none | { branch: "...", base: "..." }
send: false                            # true = send the message right away

suggest:
  - when: issues
    assignee: me
    labels_all: [agent]
    board: { project: acme/12, column: Refinement }
    triage: >
      Offer this for tickets that describe a concrete change. Not for epics,
      questions, or tickets that already contain a section "Plan".
---
Write an implementation plan for {{issue.url}} ({{issue.repo}}).

Read the ticket and its comments first. Explore the code before proposing
anything. Have a sub-agent review the plan, then add it to the ticket as a
section "Plan" and summarise open questions here.
```

The second bundled template:

```markdown
---
name: implementer
description: Implement a planned ticket and open a pull request
subject: issue
title: "#{{issue.number}} {{issue.title}}"
permission_tier: outward-tools
sandbox: workspace-write
mcp_servers: [github]
worktree: { branch: "{{me.login}}/{{issue.number}}-{{issue.slug}}" }
suggest:
  - when: issues
    assignee: me
    labels_all: [agent]
    board: { project: acme/12, column: Implementation }
---
Implement {{issue.url}} following the plan in the ticket. ...
```

Only `architect` and `implementer` ship. They are installed as editable
copies on first use, so an app update never changes a prompt silently.

**Where they live**, later wins by name: bundled → `<config_dir>/session-templates/*.md`
→ `<project>/.code-assistant/session-templates/*.md`. Project files are
trust-gated like `.mcp.json`, because a template chooses permission tier and
sandbox. A template outside a project folder applies to every project unless
it says `projects: [shop]`.

**One file, not two.** Rule and setup sit in the same file because for a
handful of personal templates "when" and "what" are edited together. If team
rules later need to share a template, a rule can move to
`suggestions.json` and name the template; the loader then accepts both.

### Front matter → session

Everything maps onto what a session already has; no new runtime concept.

| Key | Applied as |
| --- | --- |
| `model`, `effort` | `SessionModelConfig { model_name, effort }` |
| `permission_tier` | `SessionConfig::permission_tier` |
| `sandbox` | `SessionConfig::sandbox_policy` |
| `mcp_servers` | `enabled_mcp_servers` / `disabled_mcp_servers` (the rest of the configured servers off) |
| `builtin_tools` | `enabled_builtin_tools` / `disabled_builtin_tools` |
| `worktree` | `SessionService::create_worktree` in the subject's repository, then `worktree_path` / `branch` |
| `title` | session name (`name_session` stays off for it) |
| body | the input draft, or `send_user_message` when `send: true` |

New on `SessionConfig`: `created_from: Option<TemplateOrigin { template,
subject_url }>`. It marks the session in the sidebar with the subject
(`#42`), and it is how the app knows a suggestion was taken.

An unknown model, MCP server or tool group does not block the session: it
starts with the default and says so in a notice above the input.

### Placeholders

A fixed set of `{{name}}` values, replaced as text. No loops or conditions,
so no template engine. A name the subject type does not offer is an error
when the file is loaded, shown in the settings, not when it is used.

| Subject | Placeholders |
| --- | --- |
| any | `project.name`, `me.login`, `suggestion.reason` |
| `issue` | `issue.number`, `.title`, `.url`, `.repo`, `.author`, `.labels`, `.slug`, `.body` |
| `pull_request` | `pr.number`, `.title`, `.url`, `.repo`, `.author`, `.head`, `.base`, `.slug`, `.body`, `.failed_checks` |

Prefer `.url` over `.body`: the model reads the current ticket through the
GitHub MCP server or `gh` rather than a copy pasted at suggestion time.
Where `.body` is used it is inserted inside a block marked as quoted
external text. Values in `title` and `worktree.branch` are sanitised
(`slug` is lowercase ASCII with dashes, at most 40 characters).

## Suggestion rules

`suggest:` is a list; any entry that matches produces the suggestion.

| `when` | Candidates |
| --- | --- |
| `issues` | open issues in the project's repositories |
| `pull_requests` | open pull requests in the project's repositories |

Filters, all optional, all must hold:

| Filter | Values |
| --- | --- |
| `repos` | limit to some of the project's repositories |
| `assignee`, `author` | `me`, a login, `any`, `none` |
| `review_requested` | `me` (pull requests) |
| `labels_all`, `labels_any`, `labels_none` | label names |
| `board` | `{ project: owner/number, column: name }` |
| `draft` | `true` / `false` (default `false`) |
| `checks` | `failing` / `passing` / `pending` (pull requests) |
| `updated_within_days` | number |

The project's repositories are found the way the Review panel finds them
(the folder or its direct children) and mapped to GitHub by their `origin`
remote. A project without a GitHub remote has no suggestions.

Always applied: no suggestion for a subject that already has a session from
this template (the popover offers *Open* instead), none for a dismissed
subject until it changes.

**Triage** (optional, per rule). Without `triage:` every candidate that
passes the filters is suggested. With it, a small model gets the policy
text, the subject's title, body, labels and (for pull requests) changed
files, and answers `{ "suggest": true | false, "reason": "…" }`. No tools,
no writes. The answer is cached per subject version (`updated_at`, or the
head commit for pull requests), so a ticket costs one call until it changes.
The model for it is one setting (*Settings → Templates → Triage model*); with
none set, triage is skipped and the rule behaves as if it had no policy.

Every candidate that reached the filters is written to a small decision log
with its result and reason. The settings show it per template, which is how
"why is #58 not offered" gets answered.

## Reading GitHub

Through `gh`, like `session/pull_request.rs` today; no token handling of our
own, and "me" is the `gh` login. One query per rule and repository group
(`gh api` search for issues and pull requests; `gh api graphql` for the board
column, which needs the `read:project` scope on the `gh` login — when it is
missing the rule shows that, with the command to add it).

Refreshed when the app starts, when the "+" popover opens (if older than a
minute), every five minutes while the window is active, and on a refresh
button. No webhooks, no background host: suggestions only exist while the
app is open, which is fine because they wait for a click anyway.

When `gh` is missing or logged out, templates still work by hand; the
popover says why there are no suggestions.

## UI

**The "+" popover** (`sidebar/project_picker.rs`) lists the projects as it
does today. A project with suggestions gets them underneath:

```text
 New session in…
 ─────────────────────────────────────────────────────
 shop                                              ⏎
   ✦ Architect     #42 Coupons at checkout
   ✦ Implementer   #38 Round cart totals
   ✦ Architect     #45 Export orders as CSV
 pal
 code-assistant
   ✦ Implementer   #291 Suggested sessions
 ─────────────────────────────────────────────────────
```

- Clicking a project row starts an empty session, as today.
- Clicking a suggestion creates the session from the template and opens it
  with the first message in the input (or sent, with `send: true`).
- Hovering a suggestion shows the reason (rule or triage) and two actions:
  *Not now* (returns when the subject changes) and *Never for this*.
- More than five per project collapse into "3 more…".

**Hints that there is something**: the "+" button carries a dot while any
suggestion is unseen; in the project folders view the per-project *New
session* button (`sidebar/projects_view.rs`) is highlighted and opens the
same list for that project.

**From a template by hand**: the popover's project rows get a small menu,
*From template…* → template → for `subject: issue | pull_request` a picker
of the open ones (or a pasted URL). Same result as a suggestion. This also
covers templates with `subject: none`, which are just saved session setups.

**Settings → Templates**: the list with source (bundled copy, user,
project), validation errors, *Open file*, *Reveal folder*, *New from
`architect` / `implementer`*, the triage model, and the decision log. A form
editor is not in this scope; the file is the editor.

**Session**: the row shows the subject (`#42`) next to the title. Nothing
else differs from a normal session.

## Architecture

| Crate | Addition |
| --- | --- |
| `github` (new, layer 0) | `GitHubClient` trait; `GhCliClient` over `gh api`; issue, pull request and board-item types; search queries; recorded-response fake. `session/pull_request.rs` can move onto it later |
| `code_assistant_core::session_templates` | loader (three locations, trust), front matter types, validation, placeholder rendering, bundled `architect` and `implementer` |
| `code_assistant_core::suggestions` | `SuggestionService`: refresh loop, rule matching, filters, triage with cache, state file, decision log |
| `code_assistant_core::session` | `SessionConfig::created_from`; `SessionService` methods below; `UiEvent::SuggestionsChanged` (app-scoped) |
| `ui_gpui` | popover list, button hints, *From template…* picker, settings section, subject on the session row |

Rule matching and filtering are pure functions over fetched data, tested
with tables; only the `GitHubClient` does I/O.

```rust
// SessionService
async fn list_session_templates(&self, project: Option<String>) -> Result<Vec<TemplateInfo>>;
async fn list_suggestions(&self) -> Result<Vec<Suggestion>>;            // grouped by project in the UI
async fn refresh_suggestions(&self) -> Result<()>;
async fn dismiss_suggestion(&self, id: SuggestionId, how: Dismissal) -> Result<()>;
async fn start_from_template(&self, req: StartFromTemplate) -> Result<StartedSession>;

struct StartFromTemplate { project: String, template: String, subject_url: Option<String> }
struct StartedSession    { session_id: String, first_message: String, sent: bool, notices: Vec<String> }
```

`start_from_template` renders the template, creates the worktree if asked,
builds the `SessionConfig` from `session_config_template()` plus the front
matter, and calls `create_session_with_config`. The frontend puts
`first_message` into the session's draft through its draft store, which is
why the core returns it instead of storing it.

State in `<config_dir>/suggestions/`: `state.json` (dismissals with the
subject version they were made at, triage cache), `decisions.jsonl`
(bounded). Taken suggestions need no state: the session's `created_from`
says so.

## Plan

### Step 1 — Templates by hand

- Loader, validation, placeholders, bundled `architect` and `implementer`.
- `SessionConfig::created_from`; `start_from_template`; draft pre-fill;
  notices for unknown model / server / tool group; worktree option.
- *From template…* with a pasted issue or pull request URL (resolved with
  `gh`), *Settings → Templates* list with errors.
- Tests: front matter round trips and error messages; every placeholder;
  config mapping through `TestCore` (tier, sandbox, MCP and tool selection,
  model, effort); `MainScreenTest` for the pre-filled input.
- Usable result: one click on a template plus a URL gives a configured
  session with the prompt written.

### Step 2 — Suggestions

- `github` crate with `GhCliClient` and the fake; repository → remote mapping.
- `SuggestionService`: `issues` / `pull_requests`, filters without `board`,
  refresh triggers, dismissals, "already has a session", decision log,
  `UiEvent::SuggestionsChanged`.
- Popover list, button hints, hover actions, open-list picker for *From
  template…*.
- Tests: rule tables; dismissal returns after an update; a taken suggestion
  turns into *Open*; popover through `MainScreenTest` with the fake client.
- Usable result: an issue assigned to me with label `agent` appears under
  its project and starts the architect session with one click.

### Step 3 — Board column and triage

- `board` filter over Projects v2, scope check with a helpful message.
- Triage call, cache, triage model setting, reasons in hover and log.
- Usable result: the refinement and implementation columns each offer the
  right template; tickets without substance are not offered, with a reason.

### After this scope

The same files grow into the autonomous design without being rewritten:
`send: true` plus a non-interactive permission policy is an agent, a rule
with `mode: auto` is an automation, and review or fixer templates are more
files. See `docs/autonomous-agents-plan.md`.

## Open points

| Question | Proposed |
| --- | --- |
| Names | *session template*, *suggestion*; "agent" only once something runs unattended |
| Rule and setup in one file? | Yes for now; a separate `suggestions.json` when team rules need to share templates |
| Pre-fill or send? | Pre-fill by default, `send: true` per template |
| Does the template carry a system prompt? | No, only the first message; project instructions (`AGENTS.md`) apply as in any session |
| Worktree for a project folder with several repositories | Only in the subject's repository for now; the multi-repository workspace is part of the autonomous plan |
| Suggest for work that is not mine? | The filters allow it (`assignee: any`); the bundled templates use `assignee: me` |
