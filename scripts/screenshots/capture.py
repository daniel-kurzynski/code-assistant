#!/usr/bin/env python3
"""Regenerate the app screenshots of the docs site from screenshots.yaml.

Runs the real GPUI app on a virtual X display against a mock Anthropic API,
builds the base state, captures every shot in the light and the dark theme and
writes site/assets/images/app/<name>-{light,dark}.webp. Linux only; see
README.md for the tools it needs.

    scripts/screenshots/capture.py                 # all shots
    scripts/screenshots/capture.py --only window   # some shots
    scripts/screenshots/capture.py --hold          # keep the app running after
"""

import argparse
import glob
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import urllib.request

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import fixtures  # noqa: E402

OUT_DIR = os.path.join(REPO, "site", "assets", "images", "app")
BASED_ON = os.path.join(REPO, "site", "data", "based_on.yaml")
HOME_YAML = os.path.join(REPO, "site", "data", "home.yaml")
WORK_MARKER = ".code-assistant-screenshots"
# What counts as "the app" for the commit the screenshots are based on.
APP_PATHS = ["crates", "Cargo.toml", "Cargo.lock"]

# --- Where things are in the window (logical px, window 1600x1000) ---------
TITLE_Y = 23
THEME_TOGGLE = (133, TITLE_Y)
PANEL_BUTTONS = {"review": (1267, TITLE_Y), "files": (1345, TITLE_Y), "browser": (1423, TITLE_Y)}
PARK = (700, TITLE_Y)                       # empty title bar: no hover effects
SIDEBAR_TABS = {"active": (55, 71), "projects": (133, 71)}
# Projects view: the first folder row, folder rows and session rows (one line).
PROJECT_ROW_Y0, PROJECT_ROW_H, PROJECT_SESSION_ROW_H = 118, 34, 30
NEW_SESSION_X = 230                         # the "+" that shows on hover
# Active view: rows of two lines, most recent first.
ACTIVE_ROW_Y0, ACTIVE_ROW_H = 127, 48.7
INPUT = (800, 945)
CHAT = (700, 450)
# The selector row under the input, with the right panel closed.
SELECTOR_Y = 980
SELECTORS = {"model": 340, "effort": 465, "worktree": 960, "sandbox": 1123,
             "permissions": 1288, "tools": 1418}
PERMISSION_ITEMS = {"Bypass Permissions": 863, "Ask Before Outward Actions": 891,
                    "Ask Before Writes": 918, "Ask For All Tools": 946}
PERMISSION_ITEM_X = 1292


def log(msg):
    print(f"[capture] {msg}", flush=True)


def need(tool, hint):
    if not shutil.which(tool):
        sys.exit(f"missing `{tool}`: {hint} (see scripts/screenshots/README.md)")


class Display:
    """A virtual X display at the spec's scale and helpers to drive it."""

    def __init__(self, width, height, scale, log_dir):
        self.scale = scale
        self.num = next(n for n in range(90, 200) if not os.path.exists(f"/tmp/.X11-unix/X{n}")
                        and not os.path.exists(f"/tmp/.X{n}-lock"))
        self.name = f":{self.num}"
        size = f"{width * scale + 64}x{height * scale + 64}x24"
        self.proc = subprocess.Popen(["Xvfb", self.name, "-screen", "0", size, "-nolisten", "tcp", "-noreset"],
                                     stdout=open(os.path.join(log_dir, "xvfb.log"), "w"),
                                     stderr=subprocess.STDOUT)
        self.env = {**os.environ, "DISPLAY": self.name}
        for _ in range(50):
            if os.path.exists(f"/tmp/.X11-unix/X{self.num}"):
                break
            time.sleep(0.1)
        # GPUI on X11 takes its scale factor from Xft.dpi.
        subprocess.run(["xrdb", "-nocpp", "-merge"], input=f"Xft.dpi: {96 * scale}\n", text=True,
                       env=self.env, check=True)
        self.window = None

    def run(self, *args, capture=False):
        return subprocess.run(args, env=self.env, check=True, text=True,
                              capture_output=capture).stdout

    def wait_for_window(self, title, timeout=60):
        end = time.time() + timeout
        while time.time() < end:
            r = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", f"^{title}$"],
                               env=self.env, capture_output=True, text=True)
            if r.returncode == 0 and r.stdout.split():
                self.window = r.stdout.split()[0]
                return
            time.sleep(0.5)
        raise RuntimeError(f"no window '{title}' after {timeout}s")

    def _xy(self, x, y):
        return str(round(x * self.scale)), str(round(y * self.scale))

    def move(self, x, y):
        self.run("xdotool", "mousemove", "--window", self.window, *self._xy(x, y))
        time.sleep(0.2)

    def click(self, x, y, wait=0.5):
        self.move(x, y)
        self.run("xdotool", "click", "1")
        time.sleep(wait)

    def scroll(self, x, y, up=True, times=40):
        self.move(x, y)
        self.run("xdotool", "click", "--repeat", str(times), "--delay", "15", "4" if up else "5")
        time.sleep(0.6)

    def type(self, text):
        self.run("xdotool", "type", "--delay", "12", text)

    def key(self, name):
        self.run("xdotool", "key", name)

    def shot(self, path):
        self.run("import", "-window", self.window, path)

    def close(self):
        self.proc.terminate()


class Mock:
    def __init__(self, port, speed, log_dir):
        self.port = port
        self.proc = subprocess.Popen(
            [sys.executable, os.path.join(HERE, "mock_anthropic.py"), "--port", str(port),
             "--speed", str(speed)],
            stdout=open(os.path.join(log_dir, "mock.log"), "w"), stderr=subprocess.STDOUT)
        self.status()

    def status(self, timeout=10):
        end = time.time() + timeout
        while True:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{self.port}/__status", timeout=2) as r:
                    return json.load(r)
            except OSError:
                if time.time() > end:
                    raise
                time.sleep(0.2)

    def wait_done(self, conversation, count, timeout=180):
        """Until `count` responses of the conversation finished and none streams."""
        end = time.time() + timeout
        while time.time() < end:
            s = self.status()
            if s["done"].get(conversation, 0) >= count and not s["active"].get(conversation):
                return
            time.sleep(0.3)
        raise RuntimeError(f"conversation {conversation}: fewer than {count} responses after {timeout}s "
                           f"(status {self.status()})")

    def close(self):
        self.proc.terminate()


def install_fonts(home):
    """GPUI on Linux uses "IBM Plex Sans" for the UI and the app asks for "Menlo"
    in code. Copies of Inter and JetBrains Mono under those names make the
    screenshots look like the macOS app."""
    from fontTools.ttLib import TTFont

    dest = os.path.join(home, ".local", "share", "fonts")
    os.makedirs(dest, exist_ok=True)
    roots = ["/usr/share/fonts", "/usr/local/share/fonts"]
    inter = [p for r in roots for p in glob.glob(f"{r}/**/Inter-*.[ot]tf", recursive=True)]
    mono = [p for r in roots for p in glob.glob(f"{r}/**/JetBrainsMono-*.ttf", recursive=True)
            if "Variable" not in p and "NL" not in os.path.basename(p)]
    if not inter or not mono:
        sys.exit("fonts Inter and JetBrains Mono not found (apt install fonts-inter fonts-jetbrains-mono)")
    for family, files in (("IBM Plex Sans", inter), ("Menlo", mono)):
        for src in files:
            font = TTFont(src)
            names = font["name"]
            style = names.getDebugName(17) or names.getDebugName(2) or "Regular"
            for rec in names.names:
                if rec.nameID in (1, 16):
                    rec.string = family
                elif rec.nameID == 4:
                    rec.string = f"{family} {style}"
                elif rec.nameID == 6:
                    rec.string = f"{family}-{style}".replace(" ", "")
            font.save(os.path.join(dest, f"{family.replace(' ', '')}-{os.path.basename(src)}"))


class Capture:
    def __init__(self, args):
        self.args = args
        with open(args.spec) as f:
            self.spec = yaml.safe_load(f)
        self.work = os.path.abspath(args.work)
        self.raw = os.path.join(self.work, "raw")
        self.debug_dir = os.path.join(self.work, "debug")
        self.procs = []
        # The right panel is remembered per session: session id -> panel.
        self.state = {"theme": "light", "panels": {}, "sidebar": "projects", "session": None,
                      "menu": None}
        self.step_no = 0

    # --- setup --------------------------------------------------------------
    def prepare_work_dir(self):
        if os.path.exists(self.work):
            if not os.path.exists(os.path.join(self.work, WORK_MARKER)):
                sys.exit(f"{self.work} exists and was not made by this script; pass another --work")
            shutil.rmtree(self.work)
        os.makedirs(self.raw)
        os.makedirs(self.debug_dir)
        open(os.path.join(self.work, WORK_MARKER), "w").close()

    def start(self):
        win = self.spec["window"]
        self.prepare_work_dir()
        self.paths = fixtures.create(self.work, self.args.port, HERE, win["width"], win["height"])
        home = os.path.join(self.work, "home")
        install_fonts(home)
        self.mock = Mock(self.args.port, self.args.speed, self.work)
        self.procs.append(self.mock)
        self.x = Display(win["width"], win["height"], win["scale"], self.work)
        self.procs.append(self.x)
        env = {**self.x.env,
               "HOME": home, "XDG_CONFIG_HOME": os.path.join(home, ".config"),
               "XDG_DATA_HOME": os.path.join(home, ".local", "share"),
               "CARGO_HOME": os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo")),
               "RUSTUP_HOME": os.environ.get("RUSTUP_HOME", os.path.expanduser("~/.rustup")),
               "CODE_ASSISTANT_CONFIG_DIR": self.paths["config"],
               "CODE_ASSISTANT_DATA_DIR": self.paths["data"],
               **fixtures.GIT_ENV}
        env.pop("WAYLAND_DISPLAY", None)
        for var in ("NO_PROXY", "no_proxy"):
            env[var] = ",".join(filter(None, ["127.0.0.1", "localhost", os.environ.get(var)]))
        lavapipe = "/usr/share/vulkan/icd.d/lvp_icd.json"
        if os.path.exists(lavapipe) and "VK_ICD_FILENAMES" not in env:
            env["VK_ICD_FILENAMES"] = lavapipe  # software Vulkan: same pixels on every machine
        log(f"starting {self.args.bin} on display {self.x.name}")
        self.app = subprocess.Popen([self.args.bin], cwd=self.work, env=env, start_new_session=True,
                                    stdout=open(os.path.join(self.work, "app.log"), "w"),
                                    stderr=subprocess.STDOUT)
        self.x.wait_for_window("Code Assistant")
        time.sleep(4)  # first frames, MCP servers connecting

    def stop(self):
        if getattr(self, "app", None) and self.app.poll() is None:
            os.killpg(self.app.pid, signal.SIGTERM)
            try:
                self.app.wait(5)
            except subprocess.TimeoutExpired:
                os.killpg(self.app.pid, signal.SIGKILL)
        for p in reversed(self.procs):
            p.close()

    def debug(self, label):
        if self.args.debug:
            self.step_no += 1
            self.x.shot(os.path.join(self.debug_dir, f"{self.step_no:02d}-{label}.png"))

    # --- UI state -------------------------------------------------------------
    def set_sidebar(self, view):
        if self.state["sidebar"] != view:
            self.x.click(*SIDEBAR_TABS[view], wait=1.0)
            self.state["sidebar"] = view

    def panel(self):
        return self.state["panels"].get(self.state["session"], "none")

    def set_panel(self, panel):
        current = self.panel()
        if current == panel:
            return
        self.x.click(*PANEL_BUTTONS[current if panel == "none" else panel], wait=2.0)
        self.state["panels"][self.state["session"]] = panel

    def select_session(self, session_id):
        if self.state["session"] == session_id:
            return
        self.set_sidebar("active")
        # Inbox order is most recent activity first: the reverse of creation
        # (each follow-up runs before the next session is created).
        order = [s["id"] for s in reversed(self.spec["base"]["sessions"])]
        y = ACTIVE_ROW_Y0 + ACTIVE_ROW_H * order.index(session_id)
        self.x.click(100, y, wait=2.0)
        self.state["session"] = session_id

    def open_menu(self, name):
        if self.panel() != "none":
            raise RuntimeError(f"menu {name}: selector positions assume `panel: none`")
        self.x.click(SELECTORS[name], SELECTOR_Y, wait=1.2)
        self.state["menu"] = name

    def close_menu(self):
        if self.state["menu"]:
            self.x.click(SELECTORS[self.state["menu"]], SELECTOR_Y, wait=0.8)
            self.state["menu"] = None

    def toggle_theme(self):
        self.close_menu()
        self.x.click(*THEME_TOGGLE, wait=1.5)
        self.state["theme"] = "dark" if self.state["theme"] == "light" else "light"

    def send(self, text):
        self.x.click(*INPUT, wait=0.4)
        self.x.type(text)
        self.x.key("Return")

    # --- base -----------------------------------------------------------------
    def build_base(self):
        projects = ["api-client", "web-shop"]  # sidebar folder order (fixtures.py)
        sessions_in = {p: 0 for p in projects}
        for s in self.spec["base"]["sessions"]:
            log(f"base: session {s['id']} in {s['project']}")
            self.set_sidebar("projects")
            before = projects[:projects.index(s["project"])]
            y = PROJECT_ROW_Y0 + sum(PROJECT_ROW_H + PROJECT_SESSION_ROW_H * sessions_in[p] for p in before)
            self.x.move(100, y)
            self.x.click(NEW_SESSION_X, y, wait=1.5)
            self.send(s["prompt"])
            answered = len(s["turns"])
            self.mock.wait_done(s["id"], answered)
            sessions_in[s["project"]] += 1
            self.state["session"] = s["id"]
            if s.get("commit"):
                fixtures.git(self.paths[s["project"]], "add", "-A")
                fixtures.git(self.paths[s["project"]], "commit", "-q", "-m", s["commit"])
            follow = s.get("follow_up")
            if follow:
                time.sleep(1.0)
                if follow.get("permissions"):
                    self.open_menu("permissions")
                    self.x.click(PERMISSION_ITEM_X, PERMISSION_ITEMS[follow["permissions"]], wait=1.0)
                    self.state["menu"] = None
                self.send(follow["message"])
                self.mock.wait_done(s["id"], answered + len(follow["turns"]))
            time.sleep(1.5)  # tools after the last response, rendering
            self.debug(f"base-{s['id']}")

    # --- shots ----------------------------------------------------------------
    def apply(self, shot, first):
        if shot.get("session"):
            self.select_session(shot["session"])
        if shot.get("sidebar"):
            self.set_sidebar(shot["sidebar"])
        if shot.get("panel"):
            self.set_panel(shot["panel"])
        if shot.get("scroll"):
            self.x.scroll(*CHAT, up=shot["scroll"] == "top")
        for step in shot.get("steps", []):
            if step.get("once") and not first:
                continue
            if "click" in step:
                self.x.click(*step["click"], wait=1.0)
            elif "hover" in step:
                self.x.move(*step["hover"])
            elif "wait" in step:
                time.sleep(step["wait"])
        if shot.get("menu"):
            self.open_menu(shot["menu"])
        self.x.move(*(shot.get("hover") or PARK))
        time.sleep(0.8)

    def capture_shots(self):
        only = set(self.args.only or [])
        shots = [s for s in self.spec["shots"] if not only or s["name"] in only]
        unknown = only - {s["name"] for s in self.spec["shots"]}
        if unknown:
            sys.exit(f"unknown shots: {', '.join(sorted(unknown))}")
        for shot in shots:
            # Both themes back to back, starting with the current one, so
            # `once` steps (toggles) run a single time.
            themes = [self.state["theme"], "dark" if self.state["theme"] == "light" else "light"]
            for i, theme in enumerate(themes):
                if i:
                    self.toggle_theme()
                self.apply(shot, first=i == 0)
                path = os.path.join(self.raw, f"{shot['name']}-{theme}.png")
                self.x.shot(path)
                log(f"shot {shot['name']} ({theme})")
            self.close_menu()
        return shots

    # --- output ---------------------------------------------------------------
    def write_images(self, shots):
        scale = self.spec["window"]["scale"]
        os.makedirs(self.args.out, exist_ok=True)
        convert = shutil.which("magick") or "convert"
        for shot in shots:
            for theme in ("light", "dark"):
                src = os.path.join(self.raw, f"{shot['name']}-{theme}.png")
                dst = os.path.join(self.args.out, f"{shot['name']}-{theme}.webp")
                if shot["crop"] == "full":
                    w, h = self.spec["window"]["width"], self.spec["window"]["height"]
                    out_scale = shot.get("scale", scale)
                    geometry = ["-crop", f"{w * scale}x{h * scale}+0+0", "+repage",
                                "-resize", f"{round(w * out_scale)}x{round(h * out_scale)}"]
                else:
                    x, y, w, h = (round(v * scale) for v in shot["crop"])
                    geometry = ["-crop", f"{w}x{h}+{x}+{y}", "+repage"]
                subprocess.run([convert, src, *geometry, "-strip", "-quality", "88",
                                "-define", "webp:method=6", dst], check=True)
        log(f"wrote {2 * len(shots)} images to {os.path.relpath(self.args.out, REPO)}")

    def hotspot_preview(self):
        """The tour's hotspots from site/data/home.yaml drawn on the new window shot."""
        src = os.path.join(self.raw, "window-light.png")
        if not os.path.exists(src) or not os.path.exists(HOME_YAML):
            return
        with open(HOME_YAML) as f:
            steps = yaml.safe_load(f)["tour"]["steps"]
        scale = self.spec["window"]["scale"]
        w, h = self.spec["window"]["width"] * scale, self.spec["window"]["height"] * scale
        r = 12 * scale
        draw = []
        for i, s in enumerate(steps, 1):
            cx, cy = s["x"] / 100 * w, s["y"] / 100 * h
            draw += ["-fill", "#4f5bd5", "-stroke", "white", "-strokewidth", str(2 * scale),
                     "-draw", f"circle {cx},{cy} {cx + r},{cy}",
                     "-fill", "white", "-stroke", "none", "-pointsize", str(13 * scale),
                     "-draw", f"text {cx - 4 * scale},{cy + 5 * scale} '{i}'"]
        dst = os.path.join(self.work, "tour-hotspots.png")
        subprocess.run([shutil.which("magick") or "convert", src, "-crop", f"{w}x{h}+0+0", "+repage",
                        *draw, dst], check=True)
        log(f"check the tour hotspots (site/data/home.yaml) against {dst}")

    def record_commit(self):
        """Write the app commit the screenshots show to site/data/based_on.yaml."""
        app_paths = ["--", *APP_PATHS]
        sha = subprocess.run(["git", "log", "-1", "--format=%H", *app_paths], cwd=REPO,
                             capture_output=True, text=True, check=True).stdout.strip()
        dirty = subprocess.run(["git", "status", "--porcelain", *app_paths], cwd=REPO,
                               capture_output=True, text=True, check=True).stdout.strip()
        if dirty:
            log("app sources have uncommitted changes: site/data/based_on.yaml left as it is")
            return
        with open(BASED_ON) as f:
            text = f.read()
        text, n = re.subn(r"(?m)^screenshots:.*$", f"screenshots: {sha}", text)
        if not n:
            text += f"screenshots: {sha}\n"
        with open(BASED_ON, "w") as f:
            f.write(text)
        log(f"site/data/based_on.yaml: screenshots: {sha[:12]}")

    def run(self):
        try:
            self.start()
            self.build_base()
            shots = [] if self.args.base_only else self.capture_shots()
            if shots:
                self.write_images(shots)
                self.hotspot_preview()
                if not self.args.only:
                    self.record_commit()
            if self.args.hold or self.args.base_only:
                log(f"app running on DISPLAY={self.x.name} (work dir {self.work}); Ctrl-C to stop. "
                    f"Screenshot: DISPLAY={self.x.name} import -window root shot.png")
                signal.pause()
        except KeyboardInterrupt:
            pass
        except Exception:
            try:
                self.x.shot(os.path.join(self.debug_dir, "failure.png"))
            except Exception:
                pass
            log(f"failed; logs and debug/failure.png in {self.work}")
            raise
        finally:
            self.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter,
                                     epilog=__doc__.split("\n\n", 2)[2])
    parser.add_argument("--bin", default=os.path.join(REPO, "target", "debug", "code-assistant"),
                        help="the app binary (built with `cargo build` if missing)")
    parser.add_argument("--spec", default=os.path.join(HERE, "screenshots.yaml"))
    parser.add_argument("--out", default=OUT_DIR, help="where the .webp files go")
    parser.add_argument("--work", default="/tmp/code-assistant-screenshots",
                        help="scratch dir for the demo projects, config, logs, raw captures")
    parser.add_argument("--only", nargs="+", metavar="NAME", help="capture only these shots")
    parser.add_argument("--port", type=int, default=8765, help="port of the mock API")
    parser.add_argument("--speed", type=float, default=4.0, help="play the scripted turns this much faster")
    parser.add_argument("--debug", action="store_true", help="save a screenshot after each base session")
    parser.add_argument("--hold", action="store_true", help="keep the app running when done")
    parser.add_argument("--base-only", action="store_true", help="build the base state and keep it running")
    args = parser.parse_args()

    if not sys.platform.startswith("linux"):
        sys.exit("capture.py needs Linux (Xvfb); on macOS run it in a Linux VM, container or CI")
    for tool, hint in [("Xvfb", "apt install xvfb"), ("xdotool", "apt install xdotool"),
                       ("xrdb", "apt install x11-xserver-utils"), ("import", "apt install imagemagick"),
                       ("git", "apt install git")]:
        need(tool, hint)
    if not os.path.exists(args.bin):
        log("building the app (cargo build --bin code-assistant)")
        subprocess.run(["cargo", "build", "--bin", "code-assistant"], cwd=REPO, check=True)
    Capture(args).run()


if __name__ == "__main__":
    main()
