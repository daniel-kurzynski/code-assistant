# Screenshots for the docs site

`capture.py` regenerates the app screenshots in `site/assets/images/app/`. It
runs the real app (the GPUI frontend) on a virtual X display, lets it talk to
a mock Anthropic API that replies with scripted turns, and captures each
screenshot in the light and the dark theme.

```bash
scripts/screenshots/install-deps.sh     # once, Ubuntu/Debian
scripts/screenshots/capture.py          # all screenshots, about 2 minutes plus the build
```

Linux only (Xvfb). On macOS, run it in a Linux VM or container, or later in CI.

## How it works

| File | What it is |
|---|---|
| `screenshots.yaml` | The base state (sessions and their scripted model turns) and the list of shots |
| `fixtures.py` | The demo projects (`api-client`, `web-shop`) and the app's configuration |
| `mock_anthropic.py` | Stand-in for the Anthropic API that replays the turns from `screenshots.yaml` |
| `fake_mcp.py` | A stdio MCP server with a few tools, for the Tools menu |
| `capture.py` | Builds the base in the running app, takes the shots, writes the images |

1. **Base.** In a scratch dir (`--work`, default
   `/tmp/code-assistant-screenshots`) the script creates the demo projects as
   git repositories and a config dir: an Anthropic provider pointing at the
   mock, three models, the two projects, an MCP server `jira` (connected) and
   `docs` (waiting for a login). It starts the app and creates the sessions
   of `base.sessions` in order, typing each prompt. The mock recognises a
   conversation by its prompt and answers its N-th request with turn N; the
   tools really run, so searches, diffs and `cargo test` output are real. A
   session can have a `follow_up` (switch the permission tier, send another
   message) and a `commit` (commit what it wrote in the demo project).
2. **Shots.** Each entry in `shots` names the state on top of the base: the
   session, the sidebar view, the right panel, the scroll position, extra
   clicks, an open menu, then a crop. It is captured in both themes, one right
   after the other.
3. **Output.** `<name>-light.webp` and `<name>-dark.webp` in
   `site/assets/images/app/`, used by the site's `screenshot` shortcode and the
   tour. A full run also writes the app commit to `site/data/based_on.yaml`
   (`screenshots:`), unless the app sources have uncommitted changes.
4. **Tour hotspots.** The numbered dots on the landing page sit at percent
   positions in `site/data/home.yaml`. The script draws them onto the new
   window shot (`<work>/tour-hotspots.png`); move them there if the layout
   changed.

## Options

```bash
capture.py --only window selectors   # just these shots (keeps based_on.yaml)
capture.py --debug                   # also save <work>/debug/NN-*.png after each base session
capture.py --base-only               # build the base and keep the app running
capture.py --hold                    # keep it running after the shots
capture.py --bin target/release/code-assistant
capture.py --speed 1                 # stream the turns in real time
```

With `--base-only` or `--hold`, the app stays up on the printed `DISPLAY`;
`DISPLAY=:90 import -window root shot.png` takes a look at it, and
`xdotool` clicks into it, which is how to find coordinates for a new shot.

## Adding or changing a shot

- A new screen of an existing session: add an entry to `shots` with a crop.
  Coordinates are logical pixels of the 1600×1000 window. Use `--base-only`
  and a screenshot to find them (divide pixel positions by 2).
- Something the base doesn't have yet: add a session to `base.sessions` (or
  extend one) with the turns the model should send; turns call the app's
  tools with their real input schema. Sessions in the inbox are listed most
  recent first, which `session:` relies on.
- If the app's layout changed, adjust the positions at the top of
  `capture.py` (title bar, sidebar rows, input, selector row, permission
  menu items). They assume the right panel is closed for menus.

The pages that show the screenshots use `{{< screenshot name="…" alt="…" >}}`
(`site/layouts/_shortcodes/screenshot.html`).

## Notes

- The UI font: GPUI on Linux uses *IBM Plex Sans*, and code is set in
  *Menlo*. The script makes copies of Inter and JetBrains Mono under those
  names in the scratch `HOME`, so the shots look like the macOS app.
- Rendering uses software Vulkan (Mesa lavapipe) when it is installed, so the
  output does not depend on the machine's GPU.
- A run is self-contained: the app gets its own `HOME`, config and data dirs
  in the scratch dir and never touches your own sessions or settings.
