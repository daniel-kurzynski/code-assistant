"""A local stand-in for the Anthropic Messages API that replays screenshots.yaml.

    python3 mock_anthropic.py --port 8765 [--spec screenshots.yaml] [--speed 4]

POST /v1/messages   streams the scripted turn of the conversation
GET  /v1/models     an empty model listing
GET  /__status      {"done": {id: responses finished}, "active": {id: streaming}}
/mcp                401 with an OAuth challenge, so an HTTP MCP server
                    pointing here shows as "needs login"

A conversation is recognised by its prompt (or `key`) in the first user
message; request N of it (N = assistant messages so far) gets turn N.
"""

import argparse
import itertools
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
_ids = itertools.count(1)


def render_turn(turn):
    """A turn of the spec as timed SSE lines: ([(ms, line)], hold_ms, [(ms, line)])."""
    if isinstance(turn, list):
        turn = {"blocks": turn}
    blocks = turn["blocks"]
    input_tokens = turn.get("input_tokens", 18000 + 2000 * len(blocks))
    t = turn.get("start", 600)
    events = []

    def emit(obj):
        events.append((t, "data: " + json.dumps(obj)))

    emit({"type": "message_start", "message": {
        "id": f"msg_demo{next(_ids)}", "type": "message", "role": "assistant",
        "model": "claude-sonnet-5-5", "content": [],
        "usage": {"input_tokens": input_tokens, "output_tokens": 1,
                  "cache_read_input_tokens": int(input_tokens * 0.8)}}})
    for i, block in enumerate(blocks):
        if "text" in block:
            text = block["text"]
            emit({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})
            for k in range(0, len(text), 6):
                t += 30
                emit({"type": "content_block_delta", "index": i,
                      "delta": {"type": "text_delta", "text": text[k:k + 6]}})
        else:
            emit({"type": "content_block_start", "index": i, "content_block": {
                "type": "tool_use", "id": f"toolu_demo{next(_ids)}", "name": block["tool"], "input": {}}})
            js = json.dumps(block.get("input", {}))
            for k in range(0, len(js), 24):
                t += 20
                emit({"type": "content_block_delta", "index": i,
                      "delta": {"type": "input_json_delta", "partial_json": js[k:k + 24]}})
        t += 20
        emit({"type": "content_block_stop", "index": i})
    body = events
    events = []
    t = 20
    stop = "tool_use" if any("tool" in b for b in blocks) else "end_turn"
    emit({"type": "message_delta", "delta": {"stop_reason": stop},
          "usage": {"input_tokens": input_tokens, "output_tokens": 420}})
    t += 10
    emit({"type": "message_stop"})
    return body, turn.get("hold", 0), events


def load_conversations(spec_path):
    """id -> (key, [turn]) for every base session and sub-agent."""
    with open(spec_path) as f:
        base = yaml.safe_load(f)["base"]
    conversations = {}
    for s in base["sessions"]:
        turns = list(s["turns"]) + list(s.get("follow_up", {}).get("turns", []))
        conversations[s["id"]] = (s.get("key", s["prompt"]), turns)
    for s in base.get("sub_agents", []):
        conversations[s["id"]] = (s["key"], list(s["turns"]))
    return conversations


def _text_of(content):
    if isinstance(content, str):
        return content
    return " ".join(b.get("text", "") for b in content if isinstance(b, dict))


class Handler(BaseHTTPRequestHandler):
    conversations = {}
    speed = 1.0
    lock = threading.Lock()
    done = {}
    active = {}

    def log_message(self, fmt, *args):
        sys.stderr.write("mock: " + fmt % args + "\n")

    def _empty(self, code, headers=()):
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("content-length", "0")
        self.end_headers()

    def _json(self, obj):
        body = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _mcp_challenge(self):
        port = self.server.server_address[1]
        self._empty(401, [("www-authenticate", f'Bearer resource_metadata="http://127.0.0.1:{port}'
                                               '/.well-known/oauth-protected-resource"')])

    def do_GET(self):
        if self.path.startswith("/mcp"):
            return self._mcp_challenge()
        if self.path.startswith("/.well-known"):
            return self._empty(404)
        if self.path.startswith("/__status"):
            with self.lock:
                return self._json({"done": dict(self.done), "active": dict(self.active)})
        return self._json({"data": [], "has_more": False})

    def _stream(self, events, offset):
        start = time.time()
        for at_ms, line in events:
            wait = offset + at_ms / 1000 / self.speed - (time.time() - start)
            if wait > 0:
                time.sleep(wait)
            self.wfile.write((line + "\n\n").encode())
            self.wfile.flush()

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        if self.path.startswith("/mcp"):
            return self._mcp_challenge()
        msgs = json.loads(body or b"{}").get("messages", [])
        first = next((_text_of(m["content"]) for m in msgs if m["role"] == "user"), "")
        step = sum(1 for m in msgs if m["role"] == "assistant")
        name = next((n for n, (key, _) in self.conversations.items() if key in first), None)
        turns = self.conversations[name][1] if name else []
        sys.stderr.write(f"mock: conversation={name} step={step} first={first[:60]!r}\n")
        turn = turns[step] if step < len(turns) else [{"text": "Done."}]
        head, hold, tail = render_turn(turn)
        key = name or "unknown"
        with self.lock:
            self.active[key] = self.active.get(key, 0) + 1
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        try:
            self._stream(head, 0)
            time.sleep(hold / 1000)  # not sped up: holds keep a response open
            self._stream(tail, 0)
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            with self.lock:
                self.active[key] -= 1
                self.done[key] = self.done.get(key, 0) + 1
        self.close_connection = True


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--spec", default=os.path.join(HERE, "screenshots.yaml"))
    parser.add_argument("--speed", type=float, default=1.0, help="play turns this many times faster")
    args = parser.parse_args()
    Handler.conversations = load_conversations(args.spec)
    Handler.speed = args.speed
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    server.serve_forever()


if __name__ == "__main__":
    main()
