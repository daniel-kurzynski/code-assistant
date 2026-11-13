"""Scripted Anthropic Messages API mock (SSE) for UI screenshots.

Usage: mock_llm.py <port> <request-log> [seconds-per-streamed-word]

Every request answers with the next step of STEPS, picked by how many
assistant turns the conversation already holds, so one user prompt plays
the whole scripted run against the `taskly` demo project (see run.sh).
"""
import json, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

P = "taskly"

STEPS = [
    # 1: plan + look around
    [("thinking", "The user reports that sorting crashes when a task has no due date. "
                  "Comparing None with a date raises TypeError in the sort key. Let me look at the code and tests first."),
     ("text", "I'll start by getting an overview of the project and the failing test."),
     ("tool", "name_session", {"title": "Fix sorting of undated tasks"}),
     ("tool", "update_plan", {"entries": [
         {"content": "Reproduce the failing test", "status": "in_progress", "priority": "high"},
         {"content": "Fix the sort key in sort_tasks", "status": "pending", "priority": "high"},
         {"content": "Run the test suite", "status": "pending", "priority": "medium"}]}),
     ("tool", "list_files", {"project": P, "paths": ["."]})],
    # 2: run tests
    [("text", "Let me reproduce the failure."),
     ("tool", "execute_command", {"project": P, "command_line": "python3 -m unittest -v"})],
    # 3: search + read
    [("thinking", "test_undated_tasks_go_last fails with TypeError: '<' not supported between 'NoneType' and 'datetime.date'."),
     ("tool", "search_files", {"project": P, "regex": "sort_tasks|due"}),
     ("tool", "read_files", {"project": P, "paths": ["tasks.py"]})],
    # 4: edit
    [("text", "The sort key compares `None` with `date` objects. I'll sort undated tasks after dated ones by "
              "putting a flag in front of the date."),
     ("tool", "update_plan", {"entries": [
         {"content": "Reproduce the failing test", "status": "completed", "priority": "high"},
         {"content": "Fix the sort key in sort_tasks", "status": "in_progress", "priority": "high"},
         {"content": "Run the test suite", "status": "pending", "priority": "medium"}]}),
     ("tool", "edit", {"project": P, "path": "tasks.py",
                       "old_text": "    return sorted(tasks, key=lambda t: (t.done, t.due))",
                       "new_text": "    return sorted(\n        tasks,\n        key=lambda t: (t.done, t.due is None, t.due or date.min),\n    )"})],
    # 5: re-run tests
    [("tool", "execute_command", {"project": P, "command_line": "python3 -m unittest -v"})],
    # 6: summary
    [("tool", "update_plan", {"entries": [
         {"content": "Reproduce the failing test", "status": "completed", "priority": "high"},
         {"content": "Fix the sort key in sort_tasks", "status": "completed", "priority": "high"},
         {"content": "Run the test suite", "status": "completed", "priority": "medium"}]})],
    [("text", "All three tests pass now.\n\n## What was wrong\n\n`sort_tasks` used `(t.done, t.due)` as its key. "
              "For a task without a due date that tuple contains `None`, and Python 3 refuses to compare `None` "
              "with a `date`:\n\n```\nTypeError: '<' not supported between instances of 'NoneType' and 'datetime.date'\n```\n\n"
              "## The fix\n\nThe key is now `(t.done, t.due is None, t.due or date.min)`:\n\n"
              "| Part | Effect |\n|---|---|\n| `t.done` | open tasks before finished ones |\n"
              "| `t.due is None` | dated tasks before undated ones |\n| `t.due or date.min` | earliest date first, never compares `None` |\n\n"
              "No other call sites of `sort_tasks` needed changes. Want me to add a test for two undated tasks keeping their order?")],
]


def count_tool_rounds(body):
    return sum(1 for m in body.get("messages", []) if m.get("role") == "assistant")


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        sys.stderr.write("%s\n" % (a,))

    def do_GET(self):
        data = {"data": [{"id": "claude-demo", "type": "model", "display_name": "Claude Demo",
                          "created_at": "2026-01-01T00:00:00Z"}], "has_more": False,
                "first_id": "claude-demo", "last_id": "claude-demo"}
        b = json.dumps(data).encode()
        self.send_response(200); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)

    def ev(self, name, data):
        self.wfile.write(f"event: {name}\ndata: {json.dumps(data)}\n\n".encode()); self.wfile.flush()

    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        with open(sys.argv[2], "a") as f:
            f.write(json.dumps({"path": self.path, "rounds": count_tool_rounds(body),
                                "stream": body.get("stream")}) + "\n")
        idx = count_tool_rounds(body)
        # Title-generation or other side requests: answer with short text.
        if not body.get("tools"):
            steps = [("text", "Fix sorting of undated tasks")]
        else:
            steps = STEPS[idx] if idx < len(STEPS) else [("text", "Done.")]
        delay = float(sys.argv[3]) if len(sys.argv) > 3 else 0.02
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        self.ev("message_start", {"type": "message_start", "message": {
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-demo", "content": [],
            "stop_reason": None, "usage": {"input_tokens": 18000 + 4000 * idx, "output_tokens": 1,
                                           "cache_read_input_tokens": 12000 + 3000 * idx,
                                           "cache_creation_input_tokens": 900}}})
        has_tool = False
        for i, s in enumerate(steps):
            kind = s[0]
            if kind == "thinking":
                self.ev("content_block_start", {"type": "content_block_start", "index": i,
                                                "content_block": {"type": "thinking", "thinking": "", "signature": ""}})
                for w in s[1].split(" "):
                    self.ev("content_block_delta", {"type": "content_block_delta", "index": i,
                                                    "delta": {"type": "thinking_delta", "thinking": w + " "}})
                    time.sleep(delay)
                self.ev("content_block_delta", {"type": "content_block_delta", "index": i,
                                                "delta": {"type": "signature_delta", "signature": "sig"}})
            elif kind == "text":
                self.ev("content_block_start", {"type": "content_block_start", "index": i,
                                                "content_block": {"type": "text", "text": ""}})
                for w in s[1].split(" "):
                    self.ev("content_block_delta", {"type": "content_block_delta", "index": i,
                                                    "delta": {"type": "text_delta", "text": w + " "}})
                    time.sleep(delay)
            else:
                has_tool = True
                self.ev("content_block_start", {"type": "content_block_start", "index": i,
                                                "content_block": {"type": "tool_use", "id": f"toolu_{idx}_{i}",
                                                                  "name": s[1], "input": {}}})
                self.ev("content_block_delta", {"type": "content_block_delta", "index": i,
                                                "delta": {"type": "input_json_delta",
                                                          "partial_json": json.dumps(s[2])}})
            self.ev("content_block_stop", {"type": "content_block_stop", "index": i})
        self.ev("message_delta", {"type": "message_delta",
                                  "delta": {"stop_reason": "tool_use" if has_tool else "end_turn"},
                                  "usage": {"output_tokens": 350}})
        self.ev("message_stop", {"type": "message_stop"})


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
