#!/usr/bin/env python3
"""End-to-end checks of the HTTP request path. Stdlib only.

Boots a private etcd, stub engines, real cgn-agents and a real cgn-router,
then asserts what clients actually observe: status codes, SSE framing,
usage, parameters reaching the engine, retries, auth, session affinity,
cascade gating and graceful shutdown.

Usage: request_path.py [--bin-dir DIR]     (default: target/debug)
Needs `etcd` on PATH. Exits 0 on success, 1 on failure, 77 if etcd is absent.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
STUB = ROOT / "tests/e2e/stub_engine.py"
FAILS: list[str] = []


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http(url, body=None, headers=None, timeout=15):
    """Return (status, parsed-or-text body, headers)."""
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers=headers or {})
    if data is not None:
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            status, raw, hdrs = r.status, r.read(), r.headers
    except urllib.error.HTTPError as e:
        status, raw, hdrs = e.code, e.read(), e.headers
    try:
        return status, json.loads(raw), hdrs
    except ValueError:
        return status, raw.decode(errors="replace"), hdrs


def check(name, cond, detail=""):
    print(f"{'ok  ' if cond else 'FAIL'} {name}" + (f"  [{detail}]" if not cond else ""))
    if not cond:
        FAILS.append(name)


def wait_until(pred, timeout=30, what="condition"):
    end = time.time() + timeout
    while time.time() < end:
        try:
            if pred():
                return True
        except OSError:
            pass
        time.sleep(0.1)
    raise SystemExit(f"timed out waiting for {what}")


def sse_events(raw: str):
    return [l[len("data: "):] for l in raw.split("\n") if l.startswith("data: ")]


class Stack:
    def __init__(self, bin_dir: Path):
        self.bin_dir = bin_dir
        self.dir = Path(tempfile.mkdtemp(prefix="cgn-e2e-"))
        self.procs: dict[str, subprocess.Popen] = {}
        self.logs = self.dir / "logs"
        self.logs.mkdir()
        self.etcd = free_port()
        self.http_port, self.grpc_port, self.admin_port = free_port(), free_port(), free_port()
        self.stubs: dict[str, int] = {}
        self.keys = self.dir / "keys"

    def spawn(self, name, argv, **kw):
        log = open(self.logs / f"{name}.log", "w")
        self.procs[name] = subprocess.Popen(argv, stdout=log, stderr=log, **kw)

    def cluster(self):
        return f'''[cluster]
name = "e2e"
state_backend = "etcd"
etcd_endpoints = ["http://127.0.0.1:{self.etcd}"]

[security]
require_mtls = false
'''

    def start_etcd(self):
        peer = free_port()
        self.spawn("etcd", [
            "etcd", "--data-dir", str(self.dir / "etcd"),
            "--listen-client-urls", f"http://127.0.0.1:{self.etcd}",
            "--advertise-client-urls", f"http://127.0.0.1:{self.etcd}",
            "--listen-peer-urls", f"http://127.0.0.1:{peer}",
            "--initial-advertise-peer-urls", f"http://127.0.0.1:{peer}",
            "--initial-cluster", f"default=http://127.0.0.1:{peer}",
        ])
        wait_until(lambda: http(f"http://127.0.0.1:{self.etcd}/health")[0] == 200, what="etcd")

    def start_agent(self, name, model, extra="", engine_port=None):
        """Front a new stub, or an already running engine on `engine_port`."""
        stub = engine_port or free_port()
        self.stubs[name] = stub
        if engine_port is None:
            self.spawn(f"stub-{name}", [sys.executable, str(STUB), str(stub), name])
        wait_until(lambda: http(self.stub(name, "/health"))[0] == 200, 60, f"engine {name}")
        cfg = self.dir / f"agent-{name}.toml"
        cfg.write_text(self.cluster() + f'''
[agent]
listen = "127.0.0.1:{free_port()}"
role = "both"
node_id = "{name}"
kv_uds = "{self.dir}/kv-{name}.sock"
drain_timeout = "20s"

[engine]
kind = "openai_compat"
url = "http://127.0.0.1:{stub}"

[models."{model}"]
{extra}
''')
        self.spawn(f"agent-{name}", [str(self.bin_dir / "cgn-agent"), "--config", str(cfg)],
                   env={**os.environ, "CGN_NO_KVCACHED": "1"})

    def start_router(self, extra="", first_token_timeout="3s"):
        self.keys.write_text(
            "chat-key chat,embed\n"
            "limited-key chat,model:m\n"
            "budget-key chat,budget:30\n"
            "embed-key embed\n"
        )
        cfg = self.dir / "router.toml"
        cfg.write_text(self.cluster() + f'''
[auth]
enabled = true
api_keys_file = "{self.keys}"

[router]
listen_http = "127.0.0.1:{self.http_port}"
listen_grpc = "127.0.0.1:{self.grpc_port}"
listen_admin = "127.0.0.1:{self.admin_port}"
node_id = "router"
drain_timeout = "20s"
first_token_timeout = "{first_token_timeout}"

[router.cascade]
enabled = true
confidence_threshold = -1.0

[models."router-cascade"]
cascade = ["small", "big"]
{extra}
''')
        self.spawn("router", [str(self.bin_dir / "cgn-router"), "--config", str(cfg)])
        wait_until(lambda: self.url("/healthz") and http(self.url("/healthz"))[0] == 200, what="router")

    def url(self, path):
        return f"http://127.0.0.1:{self.http_port}{path}"

    def admin(self, path):
        return f"http://127.0.0.1:{self.admin_port}{path}"

    def stub(self, name, path):
        return f"http://127.0.0.1:{self.stubs[name]}{path}"

    def behave(self, name, **kw):
        http(self.stub(name, "/behave"), kw)

    def hits(self, name):
        return http(self.stub(name, "/hits"))[1]["chat"]

    def models_ready(self, *names):
        def ready(m):
            status, body, _ = http(self.url("/v1/chat/completions"), {
                "model": m, "messages": [{"role": "user", "content": "probe"}],
            }, AUTH)
            return status == 200
        wait_until(lambda: all(ready(m) for m in names), 40, f"models {names}")

    def stop(self):
        for p in self.procs.values():
            if p.poll() is None:
                p.terminate()
        for p in self.procs.values():
            try:
                p.wait(5)
            except subprocess.TimeoutExpired:
                p.kill()

    def cleanup(self, keep):
        self.stop()
        if keep:
            print(f"logs kept in {self.logs}")
        else:
            shutil.rmtree(self.dir, ignore_errors=True)


AUTH = {"authorization": "Bearer chat-key"}
CHAT = {"model": "m", "messages": [{"role": "user", "content": "hello there"}]}


def chat(stack, headers=AUTH, **over):
    return http(stack.url("/v1/chat/completions"), {**CHAT, **over}, headers)


def test_legacy_completions(s):
    st, body, _ = http(s.url("/v1/completions"), {"model": "m", "prompt": "Once upon"}, AUTH)
    check("legacy completions accepts a prompt", st == 200, f"{st} {body}")
    check("legacy object type", body.get("object") == "text_completion", body)
    check("legacy text returned", "Hello from" in body["choices"][0]["text"], body)
    last = [http(s.stub(n, "/last"))[1] for n in ("a", "b")]
    used = [l for l in last if l.get("path") == "/v1/completions"]
    check("engine got a raw prompt, no chat template", used and used[0]["body"]["prompt"] == "Once upon", last)
    st, _, _ = http(s.url("/v1/completions"), {"model": "m", "prompt": ["a", "b"]}, AUTH)
    check("multi-prompt arrays are refused", st == 400, st)


def test_usage_and_params(s):
    st, body, _ = chat(s, max_completion_tokens=900, seed=0, frequency_penalty=0.5,
                       presence_penalty=-0.5, session_unused=1)
    check("chat 200", st == 200, body)
    u = body["usage"]
    check("usage comes from the engine", u["prompt_tokens"] == 11 and u["completion_tokens"] == 4, u)
    check("usage total adds up", u["total_tokens"] == 15, u)
    last = [http(s.stub(n, "/last"))[1] for n in ("a", "b")]
    sent = next(l["body"] for l in last if l.get("body", {}).get("max_tokens") == 900)
    check("seed 0 reaches the engine", sent.get("seed") == 0, sent)
    check("penalties reach the engine",
          sent.get("frequency_penalty") == 0.5 and sent.get("presence_penalty") == -0.5, sent)
    chat(s)
    last = [http(s.stub(n, "/last"))[1].get("body", {}) for n in ("a", "b")]
    check("unset max_tokens stays unset", any(b and "max_tokens" not in b for b in last), last)


def test_stream_framing(s):
    st, raw, hdrs = http(s.url("/v1/chat/completions"), {
        **CHAT, "stream": True, "stream_options": {"include_usage": True},
    }, AUTH)
    ev = sse_events(raw)
    check("stream 200 and terminated", st == 200 and ev[-1] == "[DONE]", ev[-2:])
    frames = [json.loads(e) for e in ev[:-1]]
    finishes = [c["finish_reason"] for f in frames for c in f["choices"] if c.get("finish_reason")]
    check("exactly one finish_reason", finishes == ["stop"], finishes)
    usage = [f["usage"] for f in frames if f.get("usage")]
    check("usage chunk when requested", usage and usage[0]["total_tokens"] == 15, usage)
    check("usage chunk has empty choices", frames[-1]["choices"] == [], frames[-1])
    _, raw, _ = http(s.url("/v1/chat/completions"), {**CHAT, "stream": True}, AUTH)
    check("no usage chunk unless requested", '"usage"' not in raw, raw[-200:])


def test_stream_failures(s):
    st, body, _ = http(s.url("/v1/chat/completions"),
                       {"model": "no-such-model", "stream": True, "messages": []}, AUTH)
    check("streaming with no nodes is 503, not an empty 200", st == 503, f"{st} {body}")
    for n in ("a", "b"):
        s.behave(n, cut_after=2)
    st, raw, _ = http(s.url("/v1/chat/completions"), {**CHAT, "stream": True}, AUTH)
    ev = sse_events(raw)
    check("mid-stream cut keeps the partial output", st == 200 and "Hello" in raw, raw[:200])
    check("mid-stream cut ends in an error event", '"error"' in ev[-1], ev[-1:])
    check("mid-stream cut has no [DONE]", "[DONE]" not in ev, ev[-2:])
    for n in ("a", "b"):
        s.behave(n, cut_after=0)


def test_retry_before_first_token(s):
    # Find the node that serves this prompt, then make only it fail.
    probe = {"messages": [{"role": "user", "content": "retry probe"}]}
    before = {n: s.hits(n) for n in ("a", "b")}
    chat(s, **probe)
    target = next(n for n in ("a", "b") if s.hits(n) > before[n])
    other = "a" if target == "b" else "b"
    s.behave(target, fail_next=1, fail_status=500)
    other_before = s.hits(other)
    st, body, _ = chat(s, **probe)
    check("500 before first token was hit",
          http(s.stub(target, "/behave"), {})[1]["fail_next"] == 0)
    check("500 before first token retries invisibly", st == 200, f"{st} {body}")
    check("retry landed on the other node", s.hits(other) == other_before + 1)

    s.behave("a", fail_next=1, fail_status=400)
    s.behave("b", fail_next=1, fail_status=400)
    before = s.hits("a") + s.hits("b")
    st, _, _ = chat(s)
    check("engine 400 reaches the client", st == 400, st)
    check("engine 400 is not retried", s.hits("a") + s.hits("b") - before == 1)
    for n in ("a", "b"):
        s.behave(n, fail_next=0)


def test_auth(s):
    st, _, _ = chat(s, headers={})
    check("no token: 401", st == 401, st)
    st, _, _ = chat(s, headers={"authorization": "Bearer embed-key"})
    check("wrong scope: 403", st == 403, st)
    st, _, _ = chat(s, headers={"authorization": "Bearer limited-key"}, model="big")
    check("wrong model: 403", st == 403, st)
    st, _, _ = chat(s, headers={"authorization": "Bearer limited-key"})
    check("allowed model: 200", st == 200, st)
    st, body, _ = http(s.url("/v1/models"), headers={"authorization": "Bearer limited-key"})
    check("/v1/models is filtered by scope", [m["id"] for m in body["data"]] == ["m"], body)


def test_budget(s):
    key = {"authorization": "Bearer budget-key"}
    st, _, _ = chat(s, headers=key)
    check("budget: first request passes", st == 200, st)
    st, _, _ = chat(s, headers=key)
    check("budget: second passes (15 of 30)", st == 200, st)
    st, body, _ = chat(s, headers=key)
    check("budget: exhausted is 429", st == 429, f"{st} {body}")
    st, body, _ = http(s.url("/v1/usage"), headers=key)
    check("/v1/usage reports spend", body.get("total_tokens") == 30 and body.get("budget_limit") == 30, body)


def test_session_affinity(s):
    picks = []
    for i in range(12):
        before = {n: s.hits(n) for n in ("a", "b")}
        http(s.url("/v1/chat/completions"), {
            "model": "m", "messages": [{"role": "user", "content": f"different prompt {i}"}],
        }, {**AUTH, "x-session-id": "conv-42"})
        picks += [n for n in ("a", "b") if s.hits(n) > before[n]]
    check("session stays on one node", len(set(picks)) == 1, picks)


def test_cascade(s):
    s.behave("small", logprob=-0.1)
    s.behave("big", logprob=-0.1)
    before = (s.hits("small"), s.hits("big"))
    st, body, _ = chat(s, model="router-cascade")
    check("confident small model answers", st == 200 and "small" in body["choices"][0]["message"]["content"], body)
    check("no escalation when confident", s.hits("big") == before[1], before)
    sent = http(s.stub("small", "/last"))[1]["body"]
    check("cascade asks the engine for logprobs", sent.get("logprobs") is True, sent)

    s.behave("small", logprob=-3.0)
    st, raw, _ = http(s.url("/v1/chat/completions"),
                      {**CHAT, "model": "router-cascade", "stream": True}, AUTH)
    check("low logprob escalates to the big model", " big" in raw, raw[:300])
    s.behave("small", logprob=-0.1)
    st, body, _ = chat(s, model="router-cascade", messages=[{"role": "user", "content": "x"}])
    check("short confident answers no longer escalate", st == 200 and "small" in json.dumps(body), body)
    metrics = http(s.admin("/metrics"))[1]
    check("confidence source is metered",
          'cgn_router_cascade_confidence_total{source="logprobs"}' in metrics)


def test_disconnect_cancels_engine(s):
    for n in ("a", "b"):
        s.behave(n, delay_ms=100, extra_tokens=50)
    sock = socket.create_connection(("127.0.0.1", s.http_port))
    payload = json.dumps({**CHAT, "stream": True}).encode()
    sock.sendall(
        b"POST /v1/chat/completions HTTP/1.1\r\nhost: x\r\ncontent-type: application/json\r\n"
        b"authorization: Bearer chat-key\r\ncontent-length: " + str(len(payload)).encode() + b"\r\n\r\n" + payload
    )
    sock.recv(4096)
    sock.close()
    time.sleep(2.5)  # well under the 5 s the whole answer would take
    aborted = sum(http(s.stub(n, "/hits"))[1]["aborted"] for n in ("a", "b"))
    check("client disconnect cancels the engine call", aborted >= 1, aborted)
    for n in ("a", "b"):
        s.behave(n, delay_ms=0, extra_tokens=0)


def test_readiness(s):
    st, _, _ = http(s.admin("/readyz"))
    check("readyz ok with live nodes (admin)", st == 200, st)


def test_metrics(s):
    """Series the Helm alerts depend on, as a real router exports them."""
    chat(s, model="no-such-model")
    chat(s, headers={"authorization": "Bearer limited-key"}, model="big")
    m = http(s.admin("/metrics"))[1]
    for name in ("cgn_router_routing_decision_seconds_bucket", "cgn_router_prefix_overlap_ratio_sum",
                 "cgn_router_chat_ttft_seconds_bucket", "cgn_cluster_nodes_total"):
        check(f"exports {name}", f"\n{name}" in m)
    check("server errors carry the 5xx label",
          'cgn_router_chat_requests_total{model="no-such-model",status="5xx"}' in m)
    time.sleep(6)  # cluster gauges refresh every 5 s
    m = http(s.admin("/metrics"))[1]
    check("ready nodes report up", 'cgn_cluster_node_up{node="a"} 1' in m)


def test_metrics_after_drain(s):
    time.sleep(6)
    m = http(s.admin("/metrics"))[1]
    check("drained node leaves or reports down",
          f'cgn_cluster_node_up{{node="{s.drained}"}} 1' not in m, s.drained)


def stream_in_background(s, result):
    def run():
        result["r"] = http(s.url("/v1/chat/completions"), {**CHAT, "stream": True}, AUTH, timeout=30)

    t = threading.Thread(target=run)
    t.start()
    return t


def test_agent_drain(s):
    for n in ("a", "b"):
        s.behave(n, delay_ms=800)
    before = {n: s.hits(n) for n in ("a", "b")}
    result = {}
    t = stream_in_background(s, result)
    time.sleep(1.0)
    busy = next(n for n in ("a", "b") if s.hits(n) > before[n])
    s.survivor = "a" if busy == "b" else "b"
    s.drained = busy
    s.procs[f"agent-{busy}"].send_signal(signal.SIGTERM)
    t.join(30)
    st, raw, _ = result["r"]
    check("stream survives agent SIGTERM", st == 200 and sse_events(raw)[-1] == "[DONE]", raw[-200:])
    code = s.procs[f"agent-{busy}"].wait(25)
    check("agent exits after draining", code == 0, code)
    s.behave(s.survivor, delay_ms=0)
    st, _, _ = chat(s)
    check("traffic moves to the remaining node", st == 200, st)


def test_router_drain(s):
    s.behave(s.survivor, delay_ms=600)
    result = {}
    t = stream_in_background(s, result)
    time.sleep(0.8)
    s.procs["router"].send_signal(signal.SIGTERM)
    time.sleep(0.3)
    st, body, _ = http(s.admin("/readyz"))
    check("readyz fails during drain", st == 503 and body == "draining", f"{st} {body}")
    t.join(30)
    st, raw, _ = result["r"]
    check("in-flight stream finishes during router drain",
          st == 200 and sse_events(raw)[-1] == "[DONE]", raw[-200:])
    code = s.procs["router"].wait(25)
    check("router exits cleanly", code == 0, code)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin-dir", default=str(ROOT / "target/debug"))
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    if not shutil.which("etcd"):
        print("SKIP: etcd not on PATH")
        return 77
    s = Stack(Path(a.bin_dir))
    try:
        s.start_etcd()
        s.start_agent("a", "m")
        s.start_agent("b", "m")
        s.start_agent("small", "small")
        s.start_agent("big", "big")
        s.start_router()
        s.models_ready("m", "small", "big")
        # Both "m" nodes must be up before retry and affinity checks mean anything.
        wait_until(lambda: len(http(s.url("/v1/models"), headers=AUTH)[1]["data"]) >= 3, what="models")
        time.sleep(6)
        for test in (test_legacy_completions, test_usage_and_params, test_stream_framing,
                     test_retry_before_first_token, test_stream_failures, test_auth, test_budget,
                     test_session_affinity, test_cascade, test_disconnect_cancels_engine,
                     test_readiness, test_metrics, test_agent_drain, test_metrics_after_drain,
                     test_router_drain):
            print(f"-- {test.__name__}")
            test(s)
    finally:
        s.cleanup(keep=a.keep or bool(FAILS))
    print(f"\n{'FAILED: ' + ', '.join(FAILS) if FAILS else 'all checks passed'}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
