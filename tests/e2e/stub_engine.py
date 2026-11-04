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
  GET  /last                 -> last request path and JSON body.
  POST /behave               -> set fault injection, JSON object:
       fail_next:  N          next N generations answer `fail_status`
       fail_status: 500       status used by fail_next
       cut_after:  K          drop the connection after K tokens
       delay_ms:   D          sleep before each token
       logprob:    L          report this logprob on each token
       extra_tokens: N        append N filler tokens to the answer
       sim_cache:  C          simulate a prefix KV cache of C chunks: the
                              first token waits sim_ms_per_chunk for every
                              prompt chunk this stub doesn't hold (for
                              routing benchmarks; not a model of any engine)
       sim_ms_per_chunk: M    prefill cost per uncached chunk

The /hits counter is how tests/e2e/multi_node_kv.sh verifies KV
prefix-affinity: run one stub per agent and check that two identical
requests land on the *same* stub.

Usage: stub_engine.py PORT [NAME]
"""
from __future__ import annotations

import hashlib
import json
import socket
import sys
import threading
import time
from collections import OrderedDict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

NAME = "stub"
HITS = {"chat": 0, "aborted": 0, "chunks_hit": 0, "chunks_total": 0}
LAST = {}
BEHAVIOUR = {"fail_next": 0, "fail_status": 500, "cut_after": 0, "delay_ms": 0, "logprob": None, "extra_tokens": 0,
             "sim_cache": 0, "sim_ms_per_chunk": 0}
CACHE: "OrderedDict[str, None]" = OrderedDict()
CHUNK_CHARS = 256
LOCK = threading.Lock()
PROMPT_TOKENS = 11


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
                self._json({"name": NAME, **HITS})
        elif self.path == "/last":
            with LOCK:
                self._json(LAST)
        else:
            self._json({"error": "not found"}, 404)

    def _sse(self, frame: dict):
        self.wfile.write(b"data: " + json.dumps(frame).encode() + b"\n\n")
        self.wfile.flush()

    def do_POST(self):  # noqa: N802
        n = int(self.headers.get("content-length", "0") or 0)
        raw = self.rfile.read(n)
        body = json.loads(raw) if raw else {}
        if self.path == "/behave":
            with LOCK:
                BEHAVIOUR.update(body)
            self._json(BEHAVIOUR)
            return
        if self.path not in ("/v1/chat/completions", "/v1/completions"):
            self._json({"error": "not found"}, 404)
            return
        with LOCK:
            HITS["chat"] += 1
            LAST.clear()
            LAST.update({"path": self.path, "body": body})
            fail = BEHAVIOUR["fail_next"] > 0
            if fail:
                BEHAVIOUR["fail_next"] -= 1
            status = BEHAVIOUR["fail_status"]
            cut_after = BEHAVIOUR["cut_after"]
            delay = BEHAVIOUR["delay_ms"] / 1000
            logprob = BEHAVIOUR["logprob"]
            extra = BEHAVIOUR["extra_tokens"]
            sim = (BEHAVIOUR["sim_cache"], BEHAVIOUR["sim_ms_per_chunk"])
        if fail:
            self._json({"error": {"message": "injected failure"}}, status)
            return
        if sim[0]:
            simulate_prefill(body, *sim)
        try:
            self._stream(body, cut_after, delay, logprob, extra)
        except (BrokenPipeError, ConnectionResetError):
            with LOCK:
                HITS["aborted"] += 1
            self.close_connection = True

    def _stream(self, body, cut_after, delay, logprob, extra):
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
        tokens = ("Hello", " from", f" {NAME}", ".") + (" more",) * extra
        for i, tok in enumerate(tokens):
            if cut_after and i == cut_after:
                self.close_connection = True
                self.connection.shutdown(socket.SHUT_RDWR)
                return
            if delay:
                time.sleep(delay)
            if chat:
                choice = {"index": 0, "delta": {"content": tok}, "finish_reason": None}
                if logprob is not None and body.get("logprobs"):
                    choice["logprobs"] = {"content": [{"token": tok, "logprob": logprob}]}
            else:
                choice = {"index": 0, "text": tok, "finish_reason": None}
            self._sse({**base, "choices": [choice]})
        done = {"index": 0, "delta": {}, "finish_reason": "stop"} if chat \
            else {"index": 0, "text": "", "finish_reason": "stop"}
        self._sse({**base, "choices": [done]})
        if (body.get("stream_options") or {}).get("include_usage"):
            completion = len(tokens)
            self._sse({**base, "choices": [], "usage": {
                "prompt_tokens": PROMPT_TOKENS,
                "completion_tokens": completion,
                "total_tokens": PROMPT_TOKENS + completion,
            }})
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


def prefix_chunks(body):
    """Chained digests of fixed-size prompt chunks, like a real prefix cache."""
    text = body.get("prompt") or "".join(str(m.get("content")) for m in body.get("messages", []))
    h = hashlib.blake2b(digest_size=16)
    out = []
    for i in range(0, len(text), CHUNK_CHARS):
        h.update(text[i:i + CHUNK_CHARS].encode())
        out.append(h.hexdigest())
    return out


def simulate_prefill(body, capacity, ms_per_chunk):
    """Sleep for the chunks past the longest cached prefix, then cache them."""
    chunks = prefix_chunks(body)
    with LOCK:
        hit = 0
        for c in chunks:
            if c not in CACHE:
                break
            CACHE.move_to_end(c)
            hit += 1
        for c in chunks[hit:]:
            CACHE[c] = None
        while len(CACHE) > capacity:
            CACHE.popitem(last=False)
        HITS["chunks_hit"] += hit
        HITS["chunks_total"] += len(chunks)
    time.sleep((len(chunks) - hit) * ms_per_chunk / 1000)


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
