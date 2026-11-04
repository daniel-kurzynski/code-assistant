---
name: update-docs
description: Update the user documentation site (site/) and its app screenshots. Use when a change adds or changes something users see or configure (a feature, a menu, a setting, a command, a CLI flag), when asked to bring the docs up to date with the code, or when screenshots need to be added or regenerated.
---

# Update the user docs and screenshots

The user documentation is the Hugo site in `site/` (published to GitHub
Pages). The notes in `docs/` are for developers and are not part of this.

## Requirements

- **Every user-visible change is documented in the same pull request.** A
  new or changed feature, menu, setting, slash command or CLI flag updates
  the matching page in `site/content/docs/` (or adds one). A pull request
  without such a change needs no docs.
- **The app is shown through real screenshots, never drawn in HTML.** Images
  come from `scripts/screenshots/capture.py`, in a light and a dark version,
  and are shown with `{{< screenshot name="…" alt="…" >}}`.
- **When the UI on a screenshot changed, the screenshot is regenerated** in
  the same pull request. A new screen that a page describes gets a new shot.
- **Wording matches the app exactly**: button, menu and setting names as
  they appear in the UI, so readers can find them.
- **Cover all frontends** where a feature exists in more than one (app,
  terminal, ACP editors), with the `{{< frontends >}}` tabs.
- **`site/data/based_on.yaml` stays true**: `docs` is the app commit the
  guides were last checked against as a whole, `screenshots` the one the
  images show.

## Case 1: documenting a change in its own pull request

1. Find the pages: search `site/content/docs/` for the feature, its menu or
   setting names. Pages are grouped by area (`start`, `work`, `organize`,
   `long`, `configure`, `frontends`, `reference`); `reference/` lists
   commands, tools, CLI flags and config files and usually needs a line too.
2. Write the change in the style of the page: short steps, the UI names in
   bold, `{{< step >}}`, `{{< callout >}}`, `{{< term >}}` for terminal
   transcripts (see `site/README.md` for the shortcodes and front matter).
3. Screenshots (see "Screenshots" below): regenerate the shots whose UI
   changed (`capture.py --only <names>`) and add shots for new screens.
4. Leave `based_on.yaml` alone: it records full reviews, not single changes
   (`--only` does not touch it either).
5. Mention in the pull request description which pages and screenshots
   changed.

## Case 2: bringing the site up to date

1. Read `site/data/based_on.yaml` and list what changed in the app since:

   ```bash
   git log --oneline <docs>..HEAD -- crates Cargo.toml Cargo.lock
   ```

2. Pick out what users see or configure (merged pull request titles help;
   refactors and internals need nothing). For each, update the pages as in
   case 1.
3. Regenerate all screenshots: `scripts/screenshots/capture.py` (no
   `--only`). It writes `screenshots:` in `based_on.yaml` itself.
4. Set `docs:` in `based_on.yaml` to the commit you checked against (the
   last commit under `crates/`, `Cargo.toml`, `Cargo.lock`; the same value
   `capture.py` wrote).

## Screenshots

- Everything is defined in `scripts/screenshots/screenshots.yaml`: a `base`
  of sessions with scripted model turns (they run real tools on the demo
  projects of `scripts/screenshots/fixtures.py`) and a list of `shots`, each
  a state on top of the base plus a crop. Read
  `scripts/screenshots/README.md` before changing it.
- A new shot: add an entry to `shots`; if the base lacks what it should show,
  extend a session or add one to `base.sessions` (turns call the app's tools
  with their real input). Start with `capture.py --base-only` and take
  screenshots of the running app to find coordinates (logical pixels of the
  1600×1000 window).
- If the app's layout moved, fix the positions at the top of `capture.py`.
  A failed run leaves logs and `debug/failure.png` in its work dir.
- After a run, look at the images you changed, and at
  `<work>/tour-hotspots.png`: the numbered dots of the landing page tour
  (`site/data/home.yaml`, percent positions) must still sit on the parts they
  describe.
- `capture.py` needs Linux with the packages of
  `scripts/screenshots/install-deps.sh`. If it can't run where you are, say
  so in the pull request and name the shots that need regenerating, rather
  than editing images by hand.

## Check

- The site builds: `cd site && hugo --gc --minify` (Hugo extended 0.158+);
  `hugo server` for a look at `http://localhost:1313/code-assistant/`.
- Every `{{< screenshot name="x" >}}` has `site/assets/images/app/x-light.webp`
  and `x-dark.webp` (the build fails otherwise).
- Links between pages are root-relative (`/docs/work/permissions/`).
