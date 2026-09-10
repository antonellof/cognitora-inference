#!/usr/bin/env python3
"""Tiny OpenAI-compatible SSE stub engine for e2e tests. Stdlib only.

Stands in for vLLM / llama.cpp behind a cgn-agent running with
`engine.kind = "openai_compat"`. Implements just enough surface for the
agent driver (rust/services/cgn-agent/src/engine/openai_http.rs):

  GET  /health               -> 200 {"status":"ok"}
  GET  /v1/models            -> 200 {"object":"list","data":[...]}
  GET  /hits                 -> 200 {"name":..., "chat": <POST count>}
  POST /v1/chat/completions  -> chat-completions SSE stream:
                                role-only chunk, a few delta.content
                                chunks, finish chunk, then [DONE].
  POST /v1/completions       -> same stream (legacy `text` field).

The /hits counter is how tests/e2e/multi_node_kv.sh verifies KV
prefix-affinity: run one stub per agent and check that two identical
requests land on the *same* stub.

Usage: stub_engine.py PORT [NAME]
"""
from __future__ import annotations

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

NAME = "stub"
HITS = {"chat": 0}
LOCK = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a, **k):  # noqa: N802 - BaseHTTPRequestHandler API
        pass

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802
        if self.path == "/health":
            self._json({"status": "ok"})
        elif self.path == "/v1/models":
            self._json({"object": "list", "data": [{"id": "stub", "object": "model"}]})
        elif self.path == "/hits":
            with LOCK:
                self._json({"name": NAME, "chat": HITS["chat"]})
        else:
            self._json({"error": "not found"}, 404)

    def _sse(self, frame: dict):
        self.wfile.write(b"data: " + json.dumps(frame).encode() + b"\n\n")
        self.wfile.flush()

    def do_POST(self):  # noqa: N802
        n = int(self.headers.get("content-length", "0") or 0)
        self.rfile.read(n)
        if self.path not in ("/v1/chat/completions", "/v1/completions"):
            self._json({"error": "not found"}, 404)
            return
        with LOCK:
            HITS["chat"] += 1
        chat = self.path == "/v1/chat/completions"
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        base = {"id": "chatcmpl-stub", "object": "chat.completion.chunk", "model": "stub"}
        if chat:
            # Role-only opener, as real chat engines emit it.
            self._sse({**base, "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": None}]})
        for tok in ("Hello", " from", f" {NAME}", "."):
            if chat:
                choice = {"index": 0, "delta": {"content": tok}, "finish_reason": None}
            else:
                choice = {"index": 0, "text": tok, "finish_reason": None}
            self._sse({**base, "choices": [choice]})
        done = {"index": 0, "delta": {}, "finish_reason": "stop"} if chat \
            else {"index": 0, "text": "", "finish_reason": "stop"}
        self._sse({**base, "choices": [done]})
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


def main() -> int:
    global NAME
    if len(sys.argv) < 2:
        print("usage: stub_engine.py PORT [NAME]", file=sys.stderr)
        return 64
    port = int(sys.argv[1])
    if len(sys.argv) > 2:
        NAME = sys.argv[2]
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"stub engine '{NAME}' listening on 127.0.0.1:{port}", file=sys.stderr)
    srv.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
