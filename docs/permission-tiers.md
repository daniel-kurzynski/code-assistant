# Permission Tiers

Permission tiers decide **when the agent asks the user before running a
tool**. They are orthogonal to the sandbox: the sandbox constrains what a
command can touch once it runs, the tier decides whether a tool call runs at
all without confirmation.

## Tiers

| Tier (serde id) | Behavior |
|---|---|
| `bypass-all` (default) | Never ask; every tool call runs without prompting. |
| `auto` | A reviewer model judges every call `write-tools` would ask about, plus outward reads. Calls it finds safe and in line with the user's request run; the user is asked about the rest, with the reviewer's concern. See [Auto mode](#auto-mode). |
| `outward-tools` | Ask before any tool tagged `outward` — tools whose effects leave the machine and are visible to third parties (sending a message, calling a remote service). The tag wins over `read_only`: reading via an outward service still leaks the request. Local writes run without prompting. Embedders tag their outward tools (e.g. per MCP server via the extra-capabilities hook). |
| `write-tools` | Ask before any tool **not** tagged `read_only` — file edits, command execution, and untagged tools (e.g. MCP tools without a read-only hint) count as writes. |
| `all-tools` | Ask before every tool call. |

The tier is stored per session in `SessionConfig.permission_tier` and takes
effect on the next agent run. A stored tier this build doesn't know (written
by a newer build, or since removed) loads as `all-tools` rather than failing
the session load or silently dropping to `bypass-all`. Answer options on each prompt are **allow
once**, **always allow this tool for this session**, and **deny**. Session
grants live on the `SessionInstance` (shared with sub-agents) and survive
across agent runs but are not persisted. A denial is returned to the LLM as
an error tool result telling it not to retry.

## Auto mode

Auto mode delegates the approval to a second model, so long tasks run without
a prompt per write while destructive or out-of-scope calls still reach the
user. It follows what Claude Code's auto mode and Codex's auto-review do, with
one difference: a call the reviewer does not approve is **put to the user**
(with the reviewer's reason), not bounced back to the agent.

Flow per tool call (`ToolPermissions::check`):

1. Read-only tools (tagged `read_only`, not `outward`) run without review.
   Session grants do not skip the review: granting `execute_command` would
   otherwise approve every later shell command. Prompts the reviewer raised
   therefore offer only *Allow once / Deny*.
2. Everything else goes to the `ToolCallReviewer`. **Allow** → the call runs.
3. **Ask**, a review error or a timeout (90 s) → the ordinary permission
   prompt, with the reviewer's concern (`PermissionRequestReason::ToolInvocation
   { concern }`, `metadata.review_concern` for frontends). A reviewer can only
   take questions off the user's plate, never deny on its own.
4. Without a mediator (headless) a call the reviewer did not approve is
   denied with its concern.

What the reviewer sees (`agent_core::runtime::review`): the user's messages
and the agent's tool calls since the last new context (budget ~24 KB, the
opening request is always kept), plus the proposed call with the tool's
description and the project directory as the trust boundary. Tool results and
the agent's prose/reasoning are left out on purpose: results are where
prompt injections arrive, and the agent's own text could talk the reviewer
into approving. The policy prompt (`code_assistant_core::agent::auto_review`)
approves routine development work and asks about destruction that is hard to
undo, exfiltration, credential hunting, weakened security, actions on shared
or remote systems, running downloaded code, and anything beyond what the user
explicitly asked for. The answer is a JSON object
`{"decision": "allow" | "ask", "reason": "…"}`; anything but a clear `allow`
asks.

**Review model**: per provider (`auto_review_model` in `providers.json`,
*Settings → Providers → Auto review*), resolved for the session's model by
`ConfigurationSystem::auto_review_model`; without one the session's model
reviews. It runs at low effort, and its client is created on the first
review of a run. A small, fast model of the same provider is a good choice:
the conversation never leaves the provider the session already uses.

Like the other tiers, auto mode is not a security boundary — combine it with
the sandbox for that. Sub-agents share the session's reviewer
(`SubAgentReviewer`), which tells it that their "user" messages come from the
parent agent and authorize nothing; since the reviewer does not see the
user's own request there, it asks more often inside sub-agents.

## Architecture

- **Decision + gate**: `tools_core::permissions` — `PermissionTier`
  classifies via the `read_only` / `outward` capability tags; `ToolPermissions`
  (tier + grant set) runs the gate. `agent_core::AgentRuntime` consults it
  in both the sequential and the parallel tool-execution path before
  dispatching a tool.
- **Prompt transport**: the existing `PermissionMediator` seam. Which
  mediator is used depends on the frontend:
  - **GPUI / terminal** (via `SessionService`):
    `SessionPermissionMediator` publishes
    `UiEvent::RequestToolPermission` on the broadcast stream and awaits
    `SessionService::respond_permission`. Open requests are included in
    session snapshots and resolve as *denied* when the user stops the agent
    or a new run starts.
  - **ACP**: `AcpPermissionMediator` uses the protocol's
    `session/request_permission` RPC.
- **Escalations stay separate**: `execute_command`'s explicit
  `ask_user_approval` (sandbox bypass) keeps using the mediator directly,
  independent of the tier.

## Frontend surfaces

- **GPUI**: "Permissions" dropdown in the input-area selector row; incoming
  requests render as a banner above the input with *Allow once / Always
  (session) / Deny*; under auto mode the banner shows the reviewer's concern.
  The review model is picked per provider in *Settings → Providers*.
- **Terminal**: `/permissions [bypass-all|auto|outward-tools|write-tools|all-tools]`
  shows or switches the tier. An incoming request opens a modal prompt above the
  composer (arrow keys + Enter: *Allow once / Always allow (session) /
  Deny*), one at a time, oldest first. Esc dismisses the prompt without
  answering; the request stays pending (info banner) and can still be
  answered with `/allow`, `/always`, or `/deny`.
- **ACP**: the tiers are advertised as session modes (`bypass-all`, `auto`,
  `outward-tools`, `write-tools`, `all-tools`) in `new_session`/`load_session`; clients
  switch via `session/set_mode`. Prompts arrive as native ACP permission
  requests.
- **MCP server / headless**: no mediator is available; under an asking tier
  a gated tool call is denied with an explanatory error, so keep
  `bypass-all` there.
