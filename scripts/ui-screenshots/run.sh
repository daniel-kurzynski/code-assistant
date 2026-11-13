#!/bin/bash
# Run the GPUI app headless on Linux (Xvfb + software Vulkan) against a
# scripted mock LLM, for screenshots in a cloud/CI container.
#
#   scripts/ui-screenshots/run.sh [workdir] [seconds-per-streamed-word]
#
# Needs: a debug build (cargo build --bin code-assistant), plus
#   apt-get install -y xvfb mesa-vulkan-drivers xdotool imagemagick
# Then drive it with xdotool and capture with
#   DISPLAY=:99 import -window root shot.png
# Send a message in a new `taskly` session to play the scripted run.
#
# Note: the app hard-codes the "Menlo" font for code/terminal output; on
# Linux that falls back to a proportional font (see docs/ui-review).
set -e
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${1:-/tmp/ui-screenshots}
DELAY=${2:-0.03}
mkdir -p "$WORK/cfg" "$WORK/home" "$WORK/demo"

# Demo project with one failing test.
if [ ! -d "$WORK/demo/taskly/.git" ]; then
  mkdir -p "$WORK/demo/taskly" && cd "$WORK/demo/taskly"
  cat > tasks.py <<'EOF'
"""Tiny task list with due dates."""
from dataclasses import dataclass
from datetime import date
from typing import Optional


@dataclass
class Task:
    title: str
    due: Optional[date] = None
    done: bool = False


def sort_tasks(tasks):
    """Open tasks first, earliest due date first, undated last."""
    return sorted(tasks, key=lambda t: (t.done, t.due))


def overdue(tasks, today):
    return [t for t in tasks if t.due and t.due < today and not t.done]
EOF
  cat > test_tasks.py <<'EOF'
import unittest
from datetime import date
from tasks import Task, sort_tasks, overdue


class SortTests(unittest.TestCase):
    def test_undated_tasks_go_last(self):
        tasks = [Task("b"), Task("a", date(2026, 1, 2))]
        self.assertEqual([t.title for t in sort_tasks(tasks)], ["a", "b"])

    def test_done_tasks_go_last(self):
        tasks = [Task("x", date(2026, 1, 1), done=True), Task("y", date(2026, 3, 1))]
        self.assertEqual([t.title for t in sort_tasks(tasks)], ["y", "x"])

    def test_overdue(self):
        tasks = [Task("old", date(2025, 1, 1)), Task("new", date(2027, 1, 1))]
        self.assertEqual([t.title for t in overdue(tasks, date(2026, 6, 1))], ["old"])


if __name__ == "__main__":
    unittest.main()
EOF
  printf '__pycache__/\n' > .gitignore
  git init -q && git add -A && git -c user.email=demo@example.com -c user.name=demo commit -qm init
fi

cat > "$WORK/cfg/projects.json" <<EOF
{"taskly": {"path": "$WORK/demo/taskly"}}
EOF
cat > "$WORK/cfg/providers.json" <<'EOF'
{"mock": {"label": "Anthropic (demo)", "provider": "anthropic",
          "config": {"api_key": "sk-demo", "base_url": "http://127.0.0.1:8765/v1"}}}
EOF
cat > "$WORK/cfg/model-selection.json" <<'EOF'
{"models": [{"provider": "mock", "id": "claude-demo", "name": "Claude Demo",
             "context_token_limit": 200000}]}
EOF

export DISPLAY=:99
pgrep -x Xvfb >/dev/null || (Xvfb :99 -screen 0 1440x900x24 >/dev/null 2>&1 &)
sleep 1
python3 -I "$ROOT/scripts/ui-screenshots/mock_llm.py" 8765 "$WORK/requests.log" "$DELAY" \
  2>"$WORK/mock.err" &

export CODE_ASSISTANT_CONFIG_DIR="$WORK/cfg"
export HOME="$WORK/home" XDG_DATA_HOME="$WORK/home/.local/share" XDG_CONFIG_HOME="$WORK/home/.config"
cd "$WORK/demo/taskly"
"$ROOT/target/debug/code-assistant" >"$WORK/app.log" 2>&1 &
sleep 6
WIN=$(xdotool search --name 'Code Assistant' | head -1)
[ -n "$WIN" ] && xdotool windowmove "$WIN" 0 0 windowsize "$WIN" 1440 900
echo "App running on DISPLAY=:99 (logs in $WORK)"
