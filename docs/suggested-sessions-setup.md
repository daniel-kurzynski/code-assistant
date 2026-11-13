# Setting up suggested sessions

Suggested sessions let code-assistant offer you a ready-made session for an
issue or pull request on GitHub: a light bulb appears left of the sidebar's
"+" button, and one click opens a session that is already set up (model,
permissions, tools, optionally a worktree) with its first message written
in the input. You read it, adjust it, and send it yourself.

This guide takes you from nothing to the first suggestion. The reference for
every option is [suggested-sessions.md](suggested-sessions.md).

Paths below use the default config folder `~/.config/code-assistant/`. If
you start the app with `--config-dir` or set `CODE_ASSISTANT_CONFIG_DIR`, use
that folder instead.

## 1. Check that your project is saved

Suggestions are looked for in the projects listed in
`~/.config/code-assistant/projects.json`, which is where a project lands
when you add it with the sidebar's *New project* button (or save a temporary
one). Each project folder is either a git repository or a folder whose direct
subfolders are repositories; both work.

Each repository needs an `origin` remote on GitHub, which is how the app
knows the server and the repository:

```bash
git -C ~/code/shop/shop-api remote get-url origin
```

Any of `git@github.com:acme/shop-api.git`,
`https://github.com/acme/shop-api.git` or the same on your company's GitHub
Enterprise host is fine.

## 2. Give the app access to GitHub

Suggestions use the same GitHub access as the pull request view: if that
view already shows your pull requests, there is nothing to do here except,
for board columns, the extra scope below.

The app finds a token on your machine and never stores one itself. The
simplest way is the GitHub CLI:

```bash
gh auth login
```

For a GitHub Enterprise server, log in to that host as well:

```bash
gh auth login --hostname github.example.com
```

If you filter by board column (see step 4), the token also needs the
`read:project` scope:

```bash
gh auth refresh --scopes read:project
```

```bash
gh auth refresh --hostname github.example.com --scopes read:project
```

Without the GitHub CLI, set a token in the environment the app starts in:
`GH_TOKEN` (or `GITHUB_TOKEN`) for github.com and GHE.com. For an Enterprise
server, `GH_ENTERPRISE_TOKEN` (or `GITHUB_ENTERPRISE_TOKEN`) together with
`GH_HOST=github.example.com`; the token is sent only to the host `GH_HOST`
names. A token needs read access to issues, pull requests and, for boards,
projects.

On macOS, an app started from the Dock or Finder does not see variables set
in your shell profile; `gh auth login` avoids that problem.

## 3. Enterprise servers

Nothing to configure: the server comes from each repository's remote, and
the API is expected at `https://<host>/api/v3` and
`https://<host>/api/graphql`, where GitHub Enterprise Server serves it. Two
things to check:

- The remote names the server by its real host name. A remote that uses an
  SSH alias from `~/.ssh/config` (`git@work:team/repo.git`) is not
  recognised; change it with
  `git remote set-url origin git@github.example.com:team/repo.git`.
- `gh auth login --hostname <host>` was run for that host (step 2).

## 4. Write your first template

Create the folder and a template file:

```bash
mkdir -p ~/.config/code-assistant/session-templates
```

Save this as `~/.config/code-assistant/session-templates/plan.md`:

```markdown
---
name: plan
description: Write an implementation plan for a ticket
subject: issue
title: "Plan #{{issue.number}} {{issue.title}}"
permission_tier: write-tools
sandbox: read-only
suggest:
  - when: issues
    assignee: me
    labels_all: [agent]
---
Write an implementation plan for {{issue.url}}.

Read the ticket and its comments first, then explore the code before you
propose anything. End with the open questions.
```

What the parts do:

- Everything between the `---` lines sets up the session and says when to
  suggest it. Leave out what you do not care about; the project's defaults
  apply.
- `subject: issue` means the template is for issues (`pull_request` for pull
  requests, `none` for a saved session setup without a subject).
- `suggest:` lists the rules. This one: open issues assigned to you with the
  label `agent`, in any repository of any saved project.
- The text below the front matter is the first message. `{{issue.url}}` and
  the other placeholders are filled in when you start the session.

To match a column on a GitHub project board, add a `board` filter to the
rule. `acme/12` is the board's owner and number, as in its URL
`https://github.com/orgs/acme/projects/12`:

```yaml
suggest:
  - when: issues
    assignee: me
    board: { project: acme/12, column: Refinement }
```

For pull requests, for example reviews requested from you:

```markdown
---
name: review
subject: pull_request
title: "Review #{{pr.number}}"
permission_tier: write-tools
suggest:
  - when: pull_requests
    review_requested: me
---
Review {{pr.url}} ({{pr.head}} into {{pr.base}}). Read the diff and the
discussion, run the tests, and list what you would change.
```

Other things a template can set: `model`, `effort`, `mcp_servers`,
`builtin_tools`, a `worktree` with a branch name, `projects` to limit it to
some projects, and `send: true` to send the message right away. All options
and placeholders are listed in [suggested-sessions.md](suggested-sessions.md).

## 5. See it work

1. Make sure GitHub has a match: an open issue assigned to you with the label
   `agent`, in one of your projects' repositories.
2. Start code-assistant (or keep it open: it looks again every five minutes).
3. The light bulb appears left of the "+" in the sidebar. It is lit, with a
   dot, while there is something you have not looked at.
4. Click it. The list shows the suggestion under its project, with the
   reason it matched.
5. Click the suggestion. A new session opens with the message in the input.
   Edit it if you like and send it.

Hover a suggestion to put it away: *Not now* hides it until the issue
changes, *Never* hides it for good. A suggestion you started stays away
while its session exists. The ⟳ button in the list's header looks again
right away.

## Troubleshooting

**No light bulb.** It only appears while there is at least one suggestion,
so problems are not shown there. Look in the app's log for lines starting
with `Suggested sessions:`; they name the cause. The usual ones:

| Message | What to do |
| --- | --- |
| `No GitHub credential for <host>` | Step 2: `gh auth login --hostname <host>`, or set the variables |
| `GitHub refused … (HTTP 401)` | The token expired or was revoked; log in again |
| `GitHub refused BoardPlacements … read:project` | `gh auth refresh --hostname <host> --scopes read:project` |
| `GitHub API rate limit exceeded on <host>` | Wait; the app backs off and tries again later |
| `…/plan.md: unknown placeholder …` | A placeholder that does not fit the template's `subject` |
| `…/plan.md: invalid front matter` | A typo in a key or YAML syntax; unknown keys are reported |
| `Could not reach GitHub at <host>` | Network, proxy or certificate trouble, or the host is not a GitHub server |

When the log shows nothing, check that:

- the project is saved (step 1), not temporary;
- the repository's remote points at GitHub by the server's real host name
  and has the right owner/name;
- the template file ends in `.md` and sits directly in `session-templates/`;
- the rule's `when` fits the `subject` (`issues` with `issue`,
  `pull_requests` with `pull_request`);
- GitHub's own search finds the issue: the app searches roughly
  `is:open is:issue repo:<owner>/<name> assignee:@me label:agent`.

**The light bulb is there but the problems are hidden.** Problems for other
hosts or templates show at the bottom of the open list.

**A suggestion does not go away.** Dismissed and started suggestions are
remembered in `suggestions.json` in the app's data folder
(`~/Library/Application Support/code-assistant/` on macOS,
`~/.local/share/code-assistant/` on Linux). Deleting that file brings back
everything you dismissed.
