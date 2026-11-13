# Autonomous agents

Status: proposal (2026-11-13). Nothing here is implemented.

## Goal

Agents that start without a user: a rule in the settings says "when *this*
happens in *that* project, run *this kind of agent*". The agent works in its
own checkout, inside a sandbox, with a fixed tool set and no one to ask, and
reports back where the work came from (a ticket, a pull request).

The target is a hybrid team working off one GitHub board:

1. A ticket gets the label `agent` and sits in **Refinement** → an *architect*
   agent reads the ticket and the code, has a sub-agent review its plan, and
   writes the plan into the ticket.
2. A human moves the ticket to **Implementation** → an *implementer* agent
   builds it on a branch and opens a pull request.
3. A pull request opens → a *reviewer* agent reviews it.
4. The pipeline fails or a reviewer comments → the session that wrote the
   pull request wakes up, fixes it or answers.

## What exists and is reused

The design adds a trigger layer and a workspace layer. It does not add a
second agent runtime (ROADMAP: "Automations and external triggers create or
wake these same objects").

| Need | Existing piece |
| --- | --- |
| Start a turn without a user, get a typed result | `SessionService::start_turn_if_idle` → `TurnHandle` → `TurnOutcome` (`session/turn.rs`) |
| Deliver an event to a running or idle session | `inject_notification`, the path wakeups and monitors use |
| Describe a run and its attempts | `agent_orchestration::runs` (`RunSpec`, `RunRecord`, `RunAttempt`), vocabulary only so far |
| Decide tool permissions without a UI | `PermissionMediator` seam, `PermissionTier`, the `outward` capability tag |
| Restrict the tool set | `toolset:<id>` groups, per-session MCP selection, `ToolScope::Custom` |
| Worktrees | `git::worktree`, `SessionConfig::worktree_path` / `branch` |
| Several repositories in one folder | `discover_review_repos` (root or its direct children) |
| Sandbox | `sandbox::SandboxPolicy`, seatbelt executor (macOS only) |
| Pull request state | `session/pull_request.rs` via `gh` |
| Triage of many sessions | lifecycle inbox, `SessionStatus`, settle on merge |
| Bundled, editable prompt packages | skills (`skills/bundled.rs`, manifest + Markdown) |
| Trust for project-committed config | `tools/mcp_trust.rs` |

Gaps this plan fills: no trigger sources, no GitHub client beyond `gh pr
view`, one worktree per session (not one per repository), no sandbox on
Linux, no container target, no non-interactive permission policy, and
`RunSpec` has no consumer.

## Concepts

```text
AgentDefinition   who: prompt, model, tools, policy, sandbox, budget
Automation        when: sources, filters, policy, mode; in which project, which agent
Candidate         a subject (issue, PR at a commit, schedule slot) a source puts forward
Decision          why a candidate ran, was suggested, or was skipped
Run               one activation = one session (RunRecord), 1..n RunAttempts
Workspace         the run's own checkout: one worktree per repository
Subject binding   (repo, issue or PR) -> the session that owns it
```

### AgentDefinition (the identity)

A Markdown file with front matter, the same shape as a skill, so the prompt
is the body and stays editable as text:

```markdown
---
name: architect
description: Turns a ticket into an implementation plan
subject: issue                        # what it works on: issue | pull_request | branch | none
model: anthropic/claude-opus-5-5
effort: high
tools:
  builtin: [web, sub-agents]          # toolset ids; file and search tools are always on
  mcp: []                             # MCP servers by config name
  github: [read, comment]             # see "GitHub tools"
  write_files: false
sub_agents: { allowed: [reviewer], max_parallel: 2, max_depth: 1 }
sandbox: { target: host, network: off }
budget: { max_wall_minutes: 30, max_tool_calls: 300, max_cost_usd: 5 }
---
You are the architect on this team. ...
```

Locations, later ones win: bundled templates → `<config_dir>/agents/*.md` →
`<project>/.code-assistant/agents/*.md` (project files need the same trust
confirmation as `.mcp.json`).

Bundled templates, offered as "New agent from template" (a copy, not a
reference, so an update never changes a running team's behaviour silently):

| Template | Tools | Writes code | Outward actions |
| --- | --- | --- | --- |
| `architect` | read, search, web, sub-agents | no | comment on / edit its ticket |
| `implementer` | all local tools, sub-agents | yes | push its branch, open a PR, comment |
| `reviewer` | read, search, run tests | no | submit a PR review |
| `pr-fixer` | all local tools | yes | push to the PR branch, reply to review threads |
| `triage` | read, search | no | label, comment |
| `maintainer` (cron) | all local tools | yes | open a PR |

### Automation (the rule)

`<config_dir>/automations.json`, keyed by project; a project may also commit
`.code-assistant/automations.json` (trust-gated, and it can only reference
agents and never widen a policy beyond what the agent definition grants).

```json
{
  "id": "refine-tickets",
  "project": "shop",
  "agent": "architect",
  "mode": "suggest",
  "sources": [
    { "type": "github_issue", "repos": ["acme/shop-api", "acme/shop-web"],
      "when": { "labels_all": ["agent"], "board": { "project": "acme/12", "column": "Refinement" } } }
  ],
  "filters": { "actors": "collaborators" },
  "policy": "Plan tickets that describe a concrete change. Skip epics, questions and tickets without acceptance criteria; say what is missing instead.",
  "prompt": "Write an implementation plan for {{issue.url}} ...",
  "outputs": "post",
  "report": { "on_start": { "react": "eyes", "add_label": "agent:working" },
              "on_success": { "remove_label": "agent:working", "add_label": "agent:planned" },
              "on_blocked": { "add_label": "agent:needs-human" } },
  "limits": { "max_runs_per_day": 20, "max_attempts_per_subject": 2 }
}
```

### When to act, and when not

A bare event ("a PR was opened") is a poor reason to spend an agent run, and
"review every PR" produces noise people learn to ignore. So a trigger is not
one thing but a path with four stops, and every stop leaves a written reason.

```text
 candidate ──▶ 1 filters ──▶ 2 triage ──▶ 3 mode ──▶ run ──▶ 4 outcome
 (subject)     harness,      small model,  manual |          done | noop |
               free          read-only     suggest | auto    blocked | handoff
      └──────────── every stop writes to the decision log ────────────┘
```

**Candidates** are subjects, not events: an issue, a pull request at a head
commit, a schedule slot. Sources put candidates forward:

| Source | Candidate | Human intent |
| --- | --- | --- |
| launcher (see below) | whatever the user picked | explicit |
| `github_mention`: `/agent review`, `@name …` in a comment | that issue or PR | explicit |
| `github_assignment`: issue assigned to, or review requested from, the agent's identity | that issue or PR | explicit |
| `github_issue` / `github_pull_request`: label, board column, opened, ready for review, pushed | each match | by convention |
| `github_checks`, `github_review`: failed check, review, comment | the PR | implicit |
| `cron` | the slot, or a sweep: "all open PRs", "issues untouched for 14 days" | none |
| later: generic webhook, file watch, "another run finished" | — | — |

The first three are how people already delegate to each other (ask, assign,
request a review), need no rule design, and are the right place to start.

**Sources describe states, not edges.** `github_issue` does not mean "a label
was just added"; it means "the issue is in this state and this automation has
not handled this state yet". A reconciler compares what it observes with a
ledger. On a desktop app that matters: what happened while the laptop was
closed is not lost, a ticket moved out of a column and back fires again, and
polling and webhooks feed the same logic. The reconciler also runs the other
way: when a subject leaves the state that started a run (ticket moved back, PR
closed), the run is stopped. For pull requests the handled state is the head
commit, so a review automation chooses one of `once`, `each_push` (debounced
by a quiet period, reviewing only the new commits) or `on_request`.

**1 — Filters (harness, deterministic, free).** Draft, author (bots, own
identity, allow/deny list), fork, base branch, labels, changed paths
(`only: ["src/payments/**"]`, `ignore: ["**/*.lock", "docs/**"]`), diff size
bounds, already handled at this commit, an open run on the same subject,
per-user and per-day rate limits, budget left. Each rejection names the rule.

**2 — Triage (small model, read-only, seconds).** The automation's `policy`
is a short text in the team's words — "review changes to auth, payments and
migrations in depth; skim the rest; skip dependency bumps and generated
code". A cheap model gets the policy, the subject's summary (title,
description, changed files with stats, labels, linked issue) and the
automation's notes, and returns:

```json
{ "decision": "act" | "skip" | "ask", "reason": "...", "depth": "quick" | "deep" }
```

`depth` selects model, effort and budget from the agent definition, so a
typo fix does not get the same run as a schema migration. `ask` becomes a
suggestion even in `auto` mode. Triage has no tools, cannot write anywhere,
and cannot override a filter, a limit or a budget. Automations without a
`policy` skip this stop.

**3 — Mode: how much the team trusts the rule yet.**

| Mode | What happens to a candidate that passed |
| --- | --- |
| `manual` | nothing on its own; the rule only pre-fills the launcher |
| `suggest` | a proposal appears next to the project's *New session* button (see "Suggested agents"): "Reviewer · #57 — touches `migrations/`", started with a click, or dismissed |
| `auto` | the run starts |

A dismissal asks for an optional reason and appends it to the automation's
notes, which triage reads. A rule is promoted to `auto` when its suggestions
have stopped being wrong; the UI shows the accept rate to make that visible.
Explicit sources (launcher, mention, assignment) always run.

The same ladder applies to what a run sends out: `outputs: draft` keeps the
review, comment or pull request in the session until a human presses
**Post**; `outputs: post` sends it. Mode and outputs are independent: an
automatic reviewer whose reviews are posted by hand is a sensible second step.

**4 — Outcome.** A run ends by calling exactly one of `finish(summary)`,
`noop(reason)`, `report_blocked(reason)` or `handoff(to, note)`. `noop` is a
success, not a failure: "nothing worth saying about this PR" posts nothing
(or one reaction), costs little, and is logged. Identity prompts state the
bar for speaking up.

**The decision log** (`automations/decisions.jsonl`) records every candidate
with the stop that ended it and the reason:

```text
#57 Add coupon table      ran      triage: touches migrations/ → deep
#58 Bump serde to 1.0.219 skipped  filter: author dependabot[bot]
#59 Fix typo in README    skipped  triage: docs only
#60 WIP checkout rewrite  skipped  filter: draft
#61 Refactor cart totals  noop     agent: no findings above the bar
```

This is the answer to "why did it (not) look at this", shown per automation
and next to each subject in the launcher, with **Run anyway** on every
skipped line.

### The launcher: manual first

Before any rule exists, agents are started by hand from the project:
**New agent run** → pick the agent → the form follows the agent's `subject`.

- `pull_request` (reviewer, pr-fixer): pick the repository (when the project
  folder holds several), then a pull request from the open list — number,
  title, author, draft, checks, review state, size, and what the decision log
  says about it ("reviewed at a1b2c3d, 2 commits since").
- `issue` (architect, implementer): the open issues, filterable by label and
  board column.
- `branch`: a local or remote branch. `none`: only a prompt.

Optional: extra instructions, depth, `outputs: draft | post`. A pasted
GitHub URL skips the picking. This needs only reading from GitHub and one
checkout, so it ships in the first phase and exercises everything a triggered
run needs — definitions, policy, workspace, sandbox, reporting — while a
human still makes every "should it run" decision. Rules then automate a
choice the team has already been making by hand.

### Suggested agents: work that waits for me

The step between picking by hand and rules that run: the app notices work
that involves *me* and offers the agent for it. Starting stays one click of
mine, like starting a new session.

An agent definition says when it is worth offering:

```yaml
# reviewer
suggest_when: [review_requested_from_me]
# pr-fixer
suggest_when: [my_pr_checks_failed, my_pr_changes_requested, my_pr_has_new_comments]
# architect / implementer
suggest_when:
  - issue_assigned_to_me: { labels_any: [agent], board_column: Refinement }
```

| Condition | Read from GitHub as |
| --- | --- |
| `review_requested_from_me` | open PRs with a review request for the `gh` login |
| `my_pr_checks_failed` | my open PRs whose head commit has a failed check |
| `my_pr_changes_requested`, `my_pr_has_new_comments` | my open PRs with a review or comments newer than my last push or reply |
| `issue_assigned_to_me` | open issues assigned to me, optionally by label and column |
| `mentioned` | comments naming me since the last look |

These are a handful of search queries per project (its repositories are
known from their remotes), polled while the app is open. "Me" is the `gh`
login, so this needs no bot account and no rule design: who is assigned,
asked for a review, or owns the PR is the team's existing way of saying whose
turn it is. The filters and the decision log apply as everywhere; triage is
optional here because a human decides.

**In the sidebar.** A project with open suggestions shows a highlighted
agent icon next to its *New session* button (`sidebar/projects_view.rs`), with
a count; the header's "+" carries the total. Clicking the icon opens a
dropdown:

```text
 ✦ Suggested for shop
 ─────────────────────────────────────────────────────────
 Reviewer    #57 Add coupon table        review requested by @maria
 PR fixer    #52 Cart totals rounding    check "test" failed · 2 new comments
 Architect   #42 Coupons at checkout     assigned to you · Refinement
 ─────────────────────────────────────────────────────────
 New agent run…                          (the launcher)
```

A click on a row starts that run, with no form: agent, subject, checkout and
`outputs` come from the definition. Hovering shows the reason and offers
*Not now* (comes back when the subject changes, e.g. a new push) and *Never
for this* (logged). A suggestion disappears by itself when its reason does:
review submitted, checks green, issue unassigned. One subject can list
several agents; started ones leave the list, and the new session appears in
the project like any other, with its agent badge.

How the pieces relate: this is the `suggest` mode of the previous section
with a built-in source ("involves me") and a home in the UI. A team rule in
`suggest` mode puts its proposals into the same dropdown; a rule in `auto`
mode skips it. A per-agent switch **Start without asking** turns a personal
suggestion into an automatic run once it has earned that, which is the
gentlest route to autonomy: nothing runs that I was not already responsible
for.

### Run = session

A run is an ordinary session with three additions to `SessionConfig`:

- `origin: SessionOrigin` — `User` (default) or `Automation { automation_id,
  run_id, subject }`;
- `agent: Option<String>` — the definition, snapshotted into the session at
  start so later edits do not change a run in flight;
- `workspace: Vec<RepoCheckout>` next to the existing `worktree_path` (which
  becomes the workspace root).

Everything else follows: journal storage, the event stream, transcripts,
sub-agents, monitors, the sidebar. `RunRecord` / `RunAttempt` get their first
real consumer; the record lives in `automations/runs/<run_id>.json` and points
at the session.

```text
Queued -> Preparing -> Running -> Succeeded
                          |  \--> Failed | TimedOut | Cancelled
                          |  \--> Blocked            (needs a human)
                          \-----> Parked             (waits for events on its subject)
Parked -> Running (event delivered) ... -> Closed (subject merged or closed)
Running -> Interrupted (host died) -> new RunAttempt or Failed, per automation
```

## Architecture

```text
 GitHub ──poll/webhook──▶ TriggerSource ─┐
 cron ───────────────────▶ TriggerSource ─┼─▶ Candidate
 launcher ───────────────▶ (skips gates) ─┘        │
                                                    ▼
                      ┌──────────── AutomationService ────────────┐
                      │ Reconciler ─▶ filters ─▶ triage ─▶ mode    │
                      │ (pure, agent_orchestration)   ─▶ RunQueue  │
                      │                    │                       │
                      │              RunDriver                     │
                      │   1 WorkspaceManager.prepare (worktrees)   │
                      │   2 ExecutionTarget.start (host | container)
                      │   3 SessionService.create_session_with_config
                      │   4 start_turn_if_idle ─▶ TurnHandle.wait  │
                      │   5 Reporter (labels, status comment)      │
                      │   6 park / settle / cleanup                │
                      └───────────────┬───────────────────────────┘
                                      ▼
                    SessionService / SessionManager (unchanged seam)
                                      │
                          EventStream ─▶ GPUI, terminal, headless log
```

### Where the code goes

| Crate | Addition |
| --- | --- |
| `github` (new, layer 0) | `GitHubClient` trait; `GhCliClient` (uses `gh api`, no token handling of our own); issue/PR/check/review/Projects v2 types; `Poller`; webhook payload parsing |
| `git` | `workspace.rs`: prepare, inspect and remove a multi-repo workspace; map a GitHub repo to a local repo by remote URL |
| `sandbox` | `ExecutionTarget` (`Host`, `Container`), network policy with an allowlist; Linux backend |
| `command_executor` | `ContainerCommandExecutor` (same `CommandExecutor` trait, `docker`/`podman exec`) |
| `agent_orchestration` | `automation` module: candidates, filters, reconciler and mode decisions, limit and circuit-breaker state machine — pure, no I/O, exhaustively tested; `RunStore` trait |
| `code_assistant_core` | `automation/`: `AutomationService`, `RunDriver`, `WorkspaceManager`, `Reporter`, config loading, JSON `RunStore`; `agents/`: definition loader and bundled templates; `tools/impls/github/`; `session/permissions`: `PolicyMediator` |
| `ui_gpui` | *Settings → Agents*, *Settings → Automations*, runs view, sidebar treatment |
| `code_assistant` | `agents` subcommand (headless host, `run`, `list`, `dry-run`) |

`AutomationService` sits beside `SessionService`, like `VoiceService`: it owns
its tokio tasks, talks to sessions only through `SessionService`, and
publishes app-scoped `UiEvent::AutomationRunsChanged`.

### Host: who is running when nobody is looking

Triggers only fire while a host process runs. Two hosts, same service:

- the GPUI app while it is open;
- `code-assistant agents serve`, headless, for a machine that stays on.

A lock file in `<config_dir>/automations/` makes exactly one process the
host; the other shows runs read-only ("Running elsewhere", which the sidebar
already knows). State-based triggers make a host that was off for a night
catch up instead of missing work; cron catches up at most one slot.

### GitHub ingestion

Polling first, webhooks second, behind one `TriggerSource` trait.

- **Polling** (v1): per watched repository, `issues?since=`, open PRs, check
  runs for PRs with a binding or a matching rule, review and issue comments
  `since`; Projects v2 items by GraphQL with their status field. Conditional
  requests (ETag) keep it cheap; default interval 60 s, backing off on rate
  limits. No public endpoint, works behind NAT, works on a laptop.
- **Webhooks** (later, mainly for the headless host): an HTTP receiver with
  signature check, fed by a GitHub App or a relay. Latency drops from a
  minute to a second; the reconciler stays the same. Note that
  `projects_v2_item` events exist only for organization webhooks.

Auth in v1 is whatever `gh` is logged in as. The plan keeps a seam for a
separate identity (machine user or GitHub App installation token), because a
hybrid team needs to see *who* commented, and because loop prevention is far
simpler when the agent is not the user.

### GitHub tools instead of credentials in the sandbox

The agent gets typed tools, tagged `outward`, executed by the host:
`github_read` (issue, PR, diff, comments, check logs), `github_comment`,
`github_update_issue` (body section, labels), `github_submit_review`,
`github_reply_review_thread`, `publish_branch`, `create_pull_request`.

Two properties make this better than `gh` in a shell:

- **Scoped to the subject.** A run triggered by issue 42 can comment on issue
  42 and on pull requests it opened, nothing else. `publish_branch` only
  pushes `agent/*` branches (or the bound PR's head branch). There is no
  merge tool.
- **No token inside the sandbox.** The process the model controls cannot push
  or call the API on its own, so network can stay off or allowlisted.

Every comment carries a hidden marker (`<!-- code-assistant run=… -->`) so the
poller recognises its own output.

### Permissions without a user

`ask_question` is not registered, and the session's `PermissionMediator` is a
`PolicyMediator` that never prompts:

| Policy | Behaviour |
| --- | --- |
| `allowlist` (default) | tools named by the definition run; anything else is denied and the denial is returned to the model as the tool result |
| `auto` (later) | as above, and a command outside the allowlist goes to a small classifier model that may allow it; denied otherwise |

Sandbox escalation (`execute_command` asking to bypass the sandbox) is always
denied. When the agent cannot proceed it calls `report_blocked(reason)`: the
run becomes `Blocked`, the reporter labels the ticket, and the session shows
as needing attention in the inbox.

Chat is off while a run is active: the input is locked like a sub-agent
transcript, with **Stop** and **Take over**. Taking over ends the run,
switches the session to `origin: User` with the normal permission tier, and
keeps workspace and history, so a human can finish what the agent started.

### Workspaces: one checkout per run, several repositories

A project folder is either a repository or a folder of repositories. A run
gets `<worktrees_root>/<project>/<run-slug>/` mirroring that layout:

```text
shop/                       shop-wt/issue-42-add-coupons/
  AGENTS.md          ──▶      AGENTS.md         (read-only copy)
  shop-api/  (repo)  ──▶      shop-api/         worktree, detached at origin/main
  shop-web/  (repo)  ──▶      shop-web/         worktree, detached at origin/main
  notes/             ──▶      (not copied unless listed in `workspace.include`)
```

- Every repository gets a worktree, fetched first, detached at the default
  branch. A branch `agent/<subject>-<slug>` is created in a repository only
  when the agent first commits there, so untouched repositories leave no
  branches behind. A ticket that needs changes in two repositories yields two
  branches with one name and two linked pull requests.
- For a PR subject the matching repository is checked out at the PR head
  instead (fork PRs: fetched by `refs/pull/N/head`, never given push rights).
- `workspace.setup` (per project or per repository) runs once inside the
  sandbox after checkout: install dependencies, warm caches.
- Cleanup rides on the lifecycle sweep: when the session settles (merged,
  closed, idle for N days) the worktrees are removed — unless one holds
  uncommitted or unpushed work, in which case it is kept and flagged.
- Known costs to design for: build output per worktree (offer a shared cache
  directory setting, e.g. `CARGO_TARGET_DIR`, `sccache`), and fixed ports
  colliding between parallel runs (expose `CA_RUN_INDEX` to the setup script).

`SessionConfig::worktree_path`/`branch` stay as the root and primary branch;
the per-repository list is new. Tools that resolve a project root keep
working because the workspace root has the project's shape.

### Sandbox and execution targets

Autonomous runs get a stricter default than interactive sessions (which
default to `DangerFullAccess`), and a run that writes code refuses to start
without a working sandbox unless the automation says `allow_unsandboxed`.

| Target | Isolation | Notes |
| --- | --- | --- |
| `host` | seatbelt on macOS; bubblewrap or Landlock on Linux (new) | writable: the workspace and temp; network off by default |
| `container` | `docker`/`podman`, workspace bind-mounted | uses the project's `devcontainer.json` (image, features, `postCreateCommand`) when present, else a configured image |

How the container fits the existing tools: file tools keep working on the
host path through `fs_explorer` (already path-validated against the
workspace); only command execution changes, through a `CommandExecutor` that
runs `exec` in the run's container. One container per run, removed with the
workspace.

Two traps to handle deliberately:

- **A worktree's `.git` is a pointer** to `<main repo>/.git/worktrees/<n>`.
  In a container that path must exist, and commits write objects into the
  main repository. Mount the main `.git` at the same absolute path, with
  `hooks/` and `config` read-only. On the host target the seatbelt profile
  currently makes `.git` read-only below writable roots; the worktree's
  gitdir and the object store need to be writable, hooks and config must not
  be (a writable hook is a sandbox escape on the next host-side git call).
  Alternative per automation: `workspace.mode: clone` (a `--reference`
  clone, fully self-contained, slower).
- **Network.** `off` breaks dependency installs; `on` allows exfiltration.
  Provide `allowlist` through an egress proxy (package registries by default,
  more hosts per project). In v1 the setup step may run with network and the
  agent turn without.

Secrets: none are passed in by default. A project may list environment
variables for the setup step only.

### Subject bindings: the PR belongs to its session

When a run opens a pull request (or is started for one), the service records
`(repo, PR) → session`. Later events for that PR do not start a new agent;
they are delivered into the bound session as a message, exactly like a
monitor line:

```text
[github] check "test (ubuntu)" failed on a1b2c3d — https://…/runs/123
[github] review by @maria: changes requested, 3 comments (use github_read to see them)
```

Idle session → a turn starts; running session → queued. The agent that wrote
the code fixes it with its context and workspace intact. Events are batched
over a quiet period (several review comments arrive as one message). For pull
requests nobody is bound to (written by humans), `github_checks` /
`github_review` automations start a fresh `pr-fixer` or `reviewer` run, which
then binds.

A binding ends when the PR is merged or closed, after
`max_attempts_per_subject` fix rounds (then: label `agent:needs-human`), or on
take-over.

### Limits, loops and cost

- Concurrency: global, per project, and one active run per subject; the rest
  queue. Sub-agent fan-out and depth come from the definition.
- Budgets per run (wall time, tool calls, tokens/cost) enforced by the driver
  through `TurnHandle::cancel`; per automation per day; a circuit breaker
  disables an automation after N consecutive failures and says so in the UI.
- Loop prevention: ignore events authored by the agent's identity or carrying
  its marker; reporter label changes never satisfy a trigger of the same
  automation; a cap on fix rounds per PR.
- Staleness: if the subject leaves the triggering state mid-run (ticket moved
  back, PR closed), the run is cancelled or allowed to finish, per automation.
- Stall detection: a run with no agent event for N minutes is stopped and
  retried with backoff as a new attempt.
- Concurrency can also be capped per board column, so ten tickets dropped
  into *Implementation* do not start ten implementers.
- Notes per automation (`automations/<id>/notes.md`): what the team told it
  by dismissing suggestions or overruling it. Written only from human
  feedback through the host, never by a run that read untrusted text.
- A global kill switch pauses all automations and stops running ones.

### Untrusted input

Ticket text, comments, PR diffs and CI logs are written by other people and
reach a model that has tools. Defences, in order of weight:

1. Who may trigger: by default only events caused by repository collaborators
   (label added by, card moved by, comment author); configurable allowlist.
2. No credentials in the sandbox; outward tools scoped to the subject; no
   merge; branch protection stays the last gate.
3. External text is wrapped in a tagged block (extending `injection.rs`) and
   the identity prompts say it is data, not instructions.
4. Fork PRs: review runs read-only, and code from a fork executes only in the
   container target.
5. Everything outward is logged in an audit file per run
   (`automations/runs/<id>.audit.jsonl`).

### UI

- **Settings → Agents**: list, "New from template", editor for front matter
  fields (model, effort, tool groups, MCP servers, GitHub rights, sandbox,
  budget) and the prompt.
- **New agent run** (launcher) in the project: agent, then a subject picker.
- **Settings → Automations** (per project): sources, filters, policy text,
  mode, outputs, agent, prompt, report actions, limits; **Dry run** shows
  which open issues and PRs match right now and why, without starting.
- **Suggestions** in the dropdown next to *New session* per project, and the
  decision log per automation.
- **Sidebar**: automation sessions carry an agent badge and the subject
  (`#42`); new statuses `Queued`, `Blocked`; a filter for "agents only".
  `Blocked` and `Failed` never recede, like `NeedsApproval` today.
- **Runs view**: automation → runs → attempts, with duration, cost, outcome
  and links to session, ticket, PR.
- Terminal UI and headless: `agents list|runs|run|dry-run|pause`.

## End to end: the refinement flow

1. Poller sees issue `acme/shop-api#42`: label `agent`, board status
   *Refinement*, moved there by a collaborator.
2. Reconciler: rule `refine-tickets` matches, no ledger entry for this state
   epoch → enqueue. Reporter adds `agent:working`.
3. Driver prepares `shop-wt/issue-42-…/` (two worktrees), starts the sandbox,
   creates a session (`origin: Automation`, agent `architect`, policy
   mediator, GitHub scope = issue 42), and starts a turn with the rendered
   prompt plus the ticket as tagged external data.
4. The architect explores, drafts, spawns a `reviewer` sub-agent on the draft,
   revises, calls `github_update_issue` to write a *Plan* section.
5. `TurnOutcome::Completed` → run `Succeeded`, labels swapped to
   `agent:planned`, ledger updated, workspace kept until the session settles.
6. A human reads the plan and moves the card to *Implementation* → rule
   `implement-tickets` starts an `implementer` run, which ends by calling
   `create_pull_request`; the PR is bound to that session and the run parks.
7. CI fails → `[github] check … failed` wakes the session; it fixes, pushes.
   A `reviewer` automation reviews the PR; its comments reach the implementer
   session the same way. Merge → binding ends, session settles, worktrees go.

## Plan

Each phase ships something usable and ends with `cargo test`, `fmt`, `clippy`
clean and a doc in `docs/`.

### Phase 1 — Agents started by hand

The whole run path with a human making every "should it run" decision.

- `agents/` loader (front matter + body, `subject`), bundled templates
  `reviewer` and `architect`, snapshot into the session; identity prompt
  through `plugins/system_prompt.rs`.
- `SessionConfig::origin` / `agent`; `PolicyMediator`; `ask_question` left
  out; outcome tools `finish`, `noop`, `report_blocked`, `handoff`.
- `RunDriver` on `start_turn_if_idle`, JSON `RunStore`, budgets via cancel,
  stall detection, `Interrupted` on startup for runs left `Running`.
- `github` crate, reading only: `GitHubClient`, `GhCliClient`, list open
  pull requests and issues, read one with diff, comments and checks;
  recorded-response fake for tests.
- Launcher in GPUI (**New agent run**, subject pickers) and
  `code-assistant agents run <agent> <url>`.
- Checkout for a picked PR: one worktree at the PR head with today's
  `git::worktree` (the multi-repository workspace comes in phase 2).
- `outputs: draft` only: the review or plan stays in the session with a
  **Post** button (`github_submit_review`, `github_comment`, host-side).
- Input locked for automation sessions; Stop and Take over.
- Tests: scripted `MockLLMProvider` runs through `TestCore`; denied tool
  reaches the model as a result; budget cancels; `noop` posts nothing;
  take-over restores chat; picker against the fake client.
- Suggested agents: `suggest_when` conditions over "involves me" queries,
  polled while the app is open; highlighted icon and dropdown next to *New
  session*; *Not now* / *Never for this*; suggestions withdrawn when their
  reason goes away; decision log (filters only, no triage yet).
  `UiEvent::AgentSuggestionsChanged` (app-scoped), suggestions held by a
  small `SuggestionService` that phase 3 folds into `AutomationService`.
- Acceptance: pick an open PR, get a review draft, post it; a review request
  for me lights the icon in that project and one click starts the reviewer.

### Phase 2 — Workspaces

- `git::workspace`: discover repositories, prepare/inspect/remove, lazy
  branch creation, PR-head checkout, remote-URL mapping.
- `SessionConfig::workspace`; `WorkspaceManager`; setup command; cleanup in
  the lifecycle sweep with the "unpushed work" guard.
- Seatbelt profile: gitdir/object-store writable, hooks and config not.
- Templates `implementer`, `pr-fixer`; `publish_branch`,
  `create_pull_request`, still started from the launcher.
- Tests: temp repositories — single repo, folder of repos, dirty cleanup
  refused, two parallel workspaces of one project.

### Phase 3 — Rules that suggest

- `agent_orchestration::automation`: candidates, filters, reconciler, limits,
  circuit breaker (pure; table-driven tests including downtime catch-up and
  "subject left the state").
- `AutomationService`, `automations.json`, `Poller` with ETags and backoff,
  host lock, kill switch, decision log.
- Sources `github_pull_request`, `github_issue` (labels; board column via
  Projects v2 GraphQL), `github_mention`; filters; modes `manual` and
  `suggest`; proposals in the inbox with Run / Dismiss / Never.
- Triage step with `policy`, `depth`, notes from dismissals.
- *Settings → Agents* and *Settings → Automations* with dry run, decision
  log view with **Run anyway**, accept rate per automation.
- Acceptance: with a reviewer rule in `suggest`, a docs-only PR and a
  dependency bump are logged as skipped with reasons, a migration PR is
  proposed as `deep`.

### Phase 4 — Rules that run

- Mode `auto`, `outputs: post`, run queue with per-subject and per-column
  concurrency, `Reporter` (reaction, labels, status comment), markers, audit
  log, tagged external data, actor checks.
- `cron` source including sweeps; `agents serve` headless host.
- Runs view, sidebar badge and statuses, `UiEvent::AutomationRunsChanged`.
- Acceptance: the refinement flow above, steps 1–5, against a test repository.

### Phase 5 — Pull requests that look after themselves

- Subject bindings, event delivery into bound sessions with batching, parked
  runs, fix-round cap; `github_reply_review_thread`; multi-repo linked PRs.
- Sources `github_checks`, `github_review`, `github_assignment` (needs the
  separate identity); review cadence `once` / `each_push` / `on_request` with
  incremental review.
- Acceptance: steps 6–7.

### Phase 6 — Stronger isolation

- `ExecutionTarget`; `ContainerCommandExecutor`; devcontainer support; one
  container per run; `.git` mount rules; `workspace.mode: clone`.
- Linux host sandbox; network `allowlist` via egress proxy.
- Write-capable automations require a sandbox from this phase on (until then:
  macOS seatbelt, or explicit `allow_unsandboxed`).

### Phase 7 — Later

Webhook receiver and GitHub App identity; `auto` policy classifier; cost
reports; generic webhook and "run finished" triggers; other trackers (Linear,
Jira) behind the same `TriggerSource`; the board as a projection of a work
graph (ROADMAP "optional work graphs").

Phase 6 can move ahead of phase 5 if isolation matters more than PR flows;
they do not depend on each other.

## What others do

Looked at on 2026-11-13; product details change quickly.

| Product | Taken from it |
| --- | --- |
| [OpenAI Symphony](https://raw.githubusercontent.com/openai/symphony/main/SPEC.md) | The tracker is the state machine: issues in `active_states` with `required_labels` are eligible; every poll tick reconciles first and stops a run whose issue left its state; per-state concurrency; stall timeout; backoff; the orchestrator only reads, the agent writes through host-side tools that keep the token; runs end in a hand-off state such as *Human Review* |
| [GitHub Agentic Workflows](https://github.github.com/gh-aw/reference/frontmatter/) | Markdown + front matter per workflow; a pre-activation step that cancels before the agent starts (actor, rate limit per user, daily credit cap); an experimental `skip-if-decision` gate; `noop` as a recorded outcome; ["safe outputs"](https://github.github.com/gh-aw/reference/safe-outputs/) — the agent proposes, a separate step with limits performs the write |
| [Cursor Automations](https://cursor.com/docs/cloud-agent/automations) | Several triggers per automation; no fork PRs; memory notes per automation (with a warning about untrusted input); run as "me" or as a service account; "do nothing" left to the prompt, which is the gap the outcome tools and triage close here |
| [Greptile](https://www.greptile.com/docs/code-review-bot/trigger-code-review.md), CodeRabbit | Review filters by label, branch, author, keyword and path; drafts off by default; incremental re-review of new commits; a review tier chosen per PR |
| [GitHub Copilot coding agent](https://docs.github.com/en/copilot/using-github-copilot/coding-agent/using-copilot-to-work-on-an-issue) | Assignment as the trigger; an 👀 reaction as the first sign of life; later issue comments are not picked up, follow-ups go on the PR |
| [OpenHands resolver](https://docs.openhands.dev/usage/how-to/github-action) | A label for "take the whole issue", a mention for "answer this comment" |
| [Claude Code routines](https://code.claude.com/docs/en/routines) | Schedule, API and GitHub triggers combinable on one routine; hourly caps on events |

## Decisions to confirm

| Question | Proposed |
| --- | --- |
| Where do runs execute when the app is closed? | Nowhere, unless `agents serve` runs on some machine; catch-up on next start |
| GitHub identity | Start with the `gh` login; plan for a bot account or GitHub App before real team use |
| Polling or webhooks | Polling first |
| May a repository carry its own automations? | Yes, trust-gated, never widening an agent's policy |
| Who moves cards between columns? | Humans; agents only label and comment (configurable later) |
| Container runtime | `docker` or `podman` CLI, `devcontainer.json` read directly; no dependency on the devcontainer CLI |
| Chat with a running agent | No; Stop or Take over |
| First step | Launcher and "involves me" suggestions (phase 1); team rules come in `suggest` before `auto` |
| Who decides whether a PR is worth a review? | Filters, then a small triage model reading the team's policy text, then the agent's own `noop`; every decision logged |
