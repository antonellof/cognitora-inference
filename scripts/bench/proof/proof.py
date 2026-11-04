#!/usr/bin/env python3
"""Measure what Cognitora adds: overhead, KV routing gain, energy.

  proof.py local            Stub engines on this machine. Proves the router's
                            own cost and routing decisions. No model runs, so
                            these numbers say nothing about real inference.
  proof.py metal            Real inference on this Mac's GPU: llama-server with
                            Metal offload, two engines, one host. Real numbers,
                            but a laptop GPU sharing its host with the client.
  proof.py gpu ...          A live cluster, driven through its real endpoints.
                            Refuses to run without a GPU, so a report labelled
                            GPU always came from one.

Writes a Markdown report with --out. Stdlib only.
"""
from __future__ import annotations

import argparse
import concurrent.futures as cf
import importlib.util
import json
import platform
import shutil
import statistics
import subprocess
import sys
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
_spec = importlib.util.spec_from_file_location("request_path", ROOT / "tests/e2e/request_path.py")
rp = importlib.util.module_from_spec(_spec)
sys.modules["request_path"] = rp
_spec.loader.exec_module(rp)

AUTH = rp.AUTH


def pct(values, p):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(len(ordered) * p / 100))]


def stream_ttft(url, body, headers=None):
    """POST a streaming chat; return (seconds to first content, total)."""
    req = urllib.request.Request(url, json.dumps({**body, "stream": True}).encode(),
                                 {"content-type": "application/json", **(headers or {})})
    start = time.perf_counter()
    first = None
    with urllib.request.urlopen(req, timeout=120) as r:
        for line in r:
            if first is None and b'"content"' in line and line.startswith(b"data: "):
                first = time.perf_counter() - start
    return first if first is not None else time.perf_counter() - start, time.perf_counter() - start


def measure_overhead(direct, routed, n, concurrency, body, headers):
    """TTFT distributions on the same engine, with and without the router."""
    def run(url, hdrs):
        with cf.ThreadPoolExecutor(concurrency) as ex:
            return [t for t, _ in ex.map(lambda _: stream_ttft(url, body, hdrs), range(n))]

    run(direct, None)  # warm connections and caches on both paths
    run(routed, headers)
    d, r = run(direct, None), run(routed, headers)
    return {
        "direct_p50_ms": statistics.median(d) * 1e3,
        "routed_p50_ms": statistics.median(r) * 1e3,
        "direct_p99_ms": pct(d, 99) * 1e3,
        "routed_p99_ms": pct(r, 99) * 1e3,
        "added_p50_ms": (statistics.median(r) - statistics.median(d)) * 1e3,
        "added_p99_ms": (pct(r, 99) - pct(d, 99)) * 1e3,
    }


def conversations(n_sessions, turns, system_chars):
    """Long per-tenant system prompts with growing history: the workload
    prefix-aware routing exists for."""
    out = []
    for s in range(n_sessions):
        system = (f"You are assistant {s}. " + "Policy text that repeats. " * 400)[:system_chars]
        history = [{"role": "system", "content": system}]
        for t in range(turns):
            history = history + [{"role": "user", "content": f"Question {t} from session {s}?"}]
            out.append({"model": "m", "messages": list(history), "max_tokens": 8})
            history = history + [{"role": "assistant", "content": f"Answer {t}."}]
    # Interleave sessions, keeping each one's turns in order.
    return [c for turn in range(turns) for c in out[turn::turns]]


def run_workload(stack, requests, concurrency, nodes):
    before = {n: rp.http(stack.stub(n, "/hits"))[1] for n in nodes}
    url = stack.url("/v1/chat/completions")
    with cf.ThreadPoolExecutor(concurrency) as ex:
        ttfts = [t for t, _ in ex.map(lambda r: stream_ttft(url, r, AUTH), requests)]
    after = {n: rp.http(stack.stub(n, "/hits"))[1] for n in nodes}
    hit = sum(after[n]["chunks_hit"] - before[n]["chunks_hit"] for n in nodes)
    total = sum(after[n]["chunks_total"] - before[n]["chunks_total"] for n in nodes)
    return {
        "cache_hit_pct": 100 * hit / max(total, 1),
        "ttft_p50_ms": statistics.median(ttfts) * 1e3,
        "ttft_p95_ms": pct(ttfts, 95) * 1e3,
        "per_node": {n: after[n]["chat"] - before[n]["chat"] for n in nodes},
    }


def local_stack(bin_dir, weights, sim=None):
    s = rp.Stack(bin_dir)
    s.start_etcd()
    nodes = ["n1", "n2", "n3", "n4"]
    for n in nodes:
        s.start_agent(n, "m")
    s.start_router(f'''
[router.score_weights]
{weights}

[router.rate_limit]
rps = 100000
burst = 100000
''')
    s.models_ready("m")
    rp.wait_until(lambda: all(rp.http(s.stub(n, "/health"))[0] == 200 for n in nodes), what="stubs")
    time.sleep(6)  # let every node register
    if sim:
        for n in nodes:
            s.behave(n, **sim)
    return s, nodes


def cmd_local(a):
    bin_dir = Path(a.bin_dir)
    results = {}

    s, nodes = local_stack(bin_dir, "kv = 0.55\nload = 0.25\npower = 0.10\ncapacity = 0.10")
    try:
        body = {"model": "m", "messages": [{"role": "user", "content": "overhead probe"}], "max_tokens": 8}
        direct = s.stub("n1", "/v1/chat/completions")
        results["overhead_c1"] = measure_overhead(direct, s.url("/v1/chat/completions"), a.n, 1, body, AUTH)
        results["overhead_c8"] = measure_overhead(direct, s.url("/v1/chat/completions"), a.n, 8, body, AUTH)
    finally:
        s.cleanup(False)

    # Same workload and simulated engines; only the routing weights differ.
    sim = {"sim_cache": a.cache_chunks, "sim_ms_per_chunk": a.ms_per_chunk}
    requests = conversations(a.sessions, a.turns, a.system_chars)
    for label, weights in (
        ("kv_aware", "kv = 0.55\nload = 0.25\npower = 0.10\ncapacity = 0.10"),
        ("load_only", "kv = 0.0\nload = 1.0\npower = 0.0\ncapacity = 0.0"),
    ):
        s, nodes = local_stack(bin_dir, weights, sim)
        try:
            results[label] = run_workload(s, requests, a.concurrency, nodes)
        finally:
            s.cleanup(False)
    results["workload"] = {"requests": len(requests), **sim, "concurrency": a.concurrency,
                           "sessions": a.sessions, "turns": a.turns}
    report(a.out, "local", results)


def start_llama(stack, name, model, slots, ctx):
    port = rp.free_port()
    stack.spawn(f"llama-{name}", ["llama-server", "-m", str(model), "-ngl", "99", "--port", str(port),
                                  "-np", str(slots), "-c", str(ctx), "--cache-prompt", "--jinja"])
    rp.wait_until(lambda: rp.http(f"http://127.0.0.1:{port}/health")[0] == 200, 120, f"llama {name}")
    return port


def metal_stack(a, weights, extra=""):
    s = rp.Stack(Path(a.bin_dir))
    s.start_etcd()
    ports = {n: start_llama(s, n, a.gguf, a.slots, a.ctx) for n in ("n1", "n2")}
    for n, port in ports.items():
        s.start_agent(n, "m", engine_port=port)
    s.start_agent("small", "small", engine_port=ports["n1"])
    s.start_agent("big", "big", engine_port=ports["n2"])
    s.start_router(f"""
[router.score_weights]
{weights}

[router.rate_limit]
rps = 100000
burst = 100000
{extra}
""", first_token_timeout="120s")
    s.models_ready("m", "small", "big")
    time.sleep(6)
    return s, ports


class PowerSampler:
    """Combined CPU+GPU power from powermetrics, which needs root."""

    def __init__(self):
        self.proc = None
        if subprocess.run(["sudo", "-n", "true"], capture_output=True).returncode == 0:
            self.proc = subprocess.Popen(
                ["sudo", "-n", "powermetrics", "--samplers", "cpu_power,gpu_power", "-i", "1000"],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)

    def stop(self):
        if not self.proc:
            return None
        subprocess.run(["sudo", "-n", "pkill", "powermetrics"], capture_output=True)
        out = self.proc.communicate()[0]
        mw = [float(l.split(":")[1].split()[0]) for l in out.splitlines()
              if l.startswith("Combined Power")]
        return statistics.mean(mw) / 1000 if mw else None


def real_checks(s, a):
    """Behaviour that stubs can't prove: a real engine's logprobs and usage."""
    out = {}
    body = {"model": "m", "messages": [{"role": "user", "content": "Name a colour."}], "max_tokens": 16}
    st, r, _ = rp.http(s.url("/v1/chat/completions"), {**body, "logprobs": True}, AUTH, timeout=60)
    content = (r["choices"][0].get("logprobs") or {}).get("content") or []
    out["client_logprobs_returned"] = st == 200 and len(content) > 0
    out["real_usage"] = r["usage"]
    st, r, _ = rp.http(s.url("/v1/chat/completions"),
                       {**body, "model": "router-cascade"}, AUTH, timeout=60)
    metrics = rp.http(s.admin("/metrics"))[1]
    out["cascade_status"] = st
    out["cascade_used_real_logprobs"] = 'cgn_router_cascade_confidence_total{source="logprobs"}' in metrics
    out["cascade_used_heuristic"] = 'cgn_router_cascade_confidence_total{source="heuristic"}' in metrics
    st, r, _ = rp.http(s.url("/v1/completions"),
                       {"model": "m", "prompt": "The capital of France is", "max_tokens": 4}, AUTH, timeout=60)
    out["legacy_completion"] = r["choices"][0]["text"] if st == 200 else f"HTTP {st}"
    return out


def cmd_metal(a):
    if not shutil.which("llama-server"):
        sys.exit("llama-server not on PATH")
    results = {}
    default = "kv = 0.55\nload = 0.25\npower = 0.10\ncapacity = 0.10"
    s, ports = metal_stack(a, default)
    try:
        results["gpu"] = subprocess.run(["llama-server", "--list-devices"], capture_output=True,
                                        text=True).stderr.strip()[-600:] + "\nOffload: -ngl 99"
        results["real_checks"] = real_checks(s, a)
        body = {"model": "m", "max_tokens": 8, "temperature": 0,
                "messages": [{"role": "user", "content": "Reply with the word ok."}]}
        results["overhead_c1"] = measure_overhead(
            f"http://127.0.0.1:{ports['n1']}/v1/chat/completions",
            s.url("/v1/chat/completions"), a.n, 1, body, AUTH)
    finally:
        s.cleanup(False)

    requests = conversations(a.sessions, a.turns, a.system_chars)
    for r in requests:
        r["temperature"] = 0
    for label, weights in (("kv_aware", default),
                           ("load_only", "kv = 0.0\nload = 1.0\npower = 0.0\ncapacity = 0.0")):
        s, ports = metal_stack(a, weights)  # fresh engines: empty caches
        try:
            base = metric_sum(scrape(s.admin("/metrics")), "cgn_router_chat_completion_tokens_total")
            power = PowerSampler()
            start = time.time()
            url = s.url("/v1/chat/completions")
            with cf.ThreadPoolExecutor(a.concurrency) as ex:
                ttfts = [t for t, _ in ex.map(lambda r: stream_ttft(url, r, AUTH), requests)]
            elapsed = time.time() - start
            watts = power.stop()
            m = scrape(s.admin("/metrics"))
            tokens = metric_sum(m, "cgn_router_chat_completion_tokens_total") - base
            results[label] = {
                "cache_hit_pct": None,
                "ttft_p50_ms": statistics.median(ttfts) * 1e3,
                "ttft_p95_ms": pct(ttfts, 95) * 1e3,
                "per_node": {"elapsed_s": round(elapsed, 1), "completion_tokens": int(tokens)},
                "mean_watts": watts,
                "joules_per_token": watts * elapsed / tokens if watts and tokens else None,
            }
        finally:
            s.cleanup(False)
    results["workload"] = {"requests": len(requests), "concurrency": a.concurrency,
                           "sessions": a.sessions, "turns": a.turns, "model": Path(a.gguf).name,
                           "engines": 2, "slots_per_engine": a.slots}
    report(a.out, "metal", results)


def have_gpu():
    return shutil.which("nvidia-smi") and subprocess.run(
        ["nvidia-smi", "-L"], capture_output=True).returncode == 0


def scrape(metrics_url):
    with urllib.request.urlopen(metrics_url, timeout=10) as r:
        text = r.read().decode()
    out = {}
    for line in text.splitlines():
        if line.startswith("#") or " " not in line:
            continue
        name, value = line.rsplit(" ", 1)
        try:
            out[name] = float(value)
        except ValueError:
            pass
    return out


def metric_sum(sample, prefix):
    return sum(v for k, v in sample.items() if k.startswith(prefix))


def cmd_gpu(a):
    if not have_gpu():
        sys.exit("refusing to run: no working nvidia-smi on this host. Run this on a GPU node.")
    headers = {"authorization": f"Bearer {a.api_key}"} if a.api_key else {}
    prompt = "Explain prefix caching in one paragraph. " * 40
    body = {"model": a.model, "messages": [{"role": "user", "content": prompt}], "max_tokens": 64}
    results = {"overhead_c1": measure_overhead(
        f"{a.engine}/v1/chat/completions", f"{a.router}/v1/chat/completions", a.n, 1, body, headers)}

    requests = [{**r, "model": a.model} for r in conversations(a.sessions, a.turns, a.system_chars)]
    before, watts, start = scrape(a.metrics), [], time.time()
    url = f"{a.router}/v1/chat/completions"
    with cf.ThreadPoolExecutor(a.concurrency) as ex:
        futures = [ex.submit(stream_ttft, url, r, headers) for r in requests]
        while not all(f.done() for f in futures):
            watts.append(metric_sum(scrape(a.metrics), "cgn_cluster_power_watts_total"))
            time.sleep(1)
    ttfts = [f.result()[0] for f in futures]
    elapsed = time.time() - start
    after = scrape(a.metrics)
    tokens = metric_sum(after, "cgn_router_chat_completion_tokens_total") - metric_sum(
        before, "cgn_router_chat_completion_tokens_total")
    mean_watts = statistics.mean(watts) if watts else 0.0
    results[a.label] = {
        "ttft_p50_ms": statistics.median(ttfts) * 1e3,
        "ttft_p95_ms": pct(ttfts, 95) * 1e3,
        "completion_tokens": tokens,
        "elapsed_s": elapsed,
        "mean_watts": mean_watts,
        "joules_per_token": mean_watts * elapsed / tokens if tokens else None,
    }
    results["gpu"] = subprocess.run(["nvidia-smi", "-L"], capture_output=True, text=True).stdout.strip()
    report(a.out, "gpu", results)


def report(path, mode, r):
    lines = [
        f"# Cognitora request-path benchmark ({mode})", "",
        f"Generated {datetime.now(timezone.utc):%Y-%m-%d %H:%M} UTC by `scripts/bench/proof/proof.py {mode}`.",
        f"Host: {platform.platform()}, {platform.machine()}.", "",
    ]
    if mode == "local":
        lines += [
            "**No model ran.** Engines are `tests/e2e/stub_engine.py`, a Python stub on the same",
            "host. This measures the router and agent, not inference. The KV table uses a *simulated*",
            "prefix cache, so it shows routing decisions, not real TTFT.", "",
        ]
    elif mode == "metal":
        lines += [
            "**Real inference on an Apple GPU (Metal), not NVIDIA and not vLLM.** Two `llama-server`",
            "processes share one laptop GPU with the load generator, so absolute numbers are low",
            "and don't predict a datacenter. Prefix reuse here is llama.cpp's own slot cache.", "",
            "GPU evidence:", "```", r.pop("gpu", ""), "```", "",
            "Real-engine checks:", "```json", json.dumps(r.pop("real_checks", {}), indent=2), "```", "",
        ]
    else:
        lines += ["GPUs:", "```", r.pop("gpu", ""), "```", ""]
    for key, title in (("overhead_c1", "Overhead, 1 client"), ("overhead_c8", "Overhead, 8 clients")):
        if key in r:
            o = r[key]
            lines += [f"## {title}", "", "| | Direct | Via router | Added |", "|---|---|---|---|",
                      f"| TTFT p50 | {o['direct_p50_ms']:.2f} ms | {o['routed_p50_ms']:.2f} ms | {o['added_p50_ms']:.2f} ms |",
                      f"| TTFT p99 | {o['direct_p99_ms']:.2f} ms | {o['routed_p99_ms']:.2f} ms | {o['added_p99_ms']:.2f} ms |", ""]
    if "kv_aware" in r and "load_only" in r:
        k, l = r["kv_aware"], r["load_only"]
        lines += ["## KV-aware routing vs load-only routing", "",
                  f"Workload: {json.dumps(r['workload'])}", "",
                  "| | KV-aware | Load-only |", "|---|---|---|",
                  *([f"| Prefix chunks served from cache | {k['cache_hit_pct']:.1f}% | {l['cache_hit_pct']:.1f}% |"]
                    if k.get("cache_hit_pct") is not None else []),
                  f"| TTFT p50 | {k['ttft_p50_ms']:.1f} ms | {l['ttft_p50_ms']:.1f} ms |",
                  f"| TTFT p95 | {k['ttft_p95_ms']:.1f} ms | {l['ttft_p95_ms']:.1f} ms |",
                  *([f"| Requests per node | {sorted(k['per_node'].values())} | {sorted(l['per_node'].values())} |"]
                    if "elapsed_s" not in k["per_node"] else
                    [f"| Elapsed | {k['per_node']['elapsed_s']} s | {l['per_node']['elapsed_s']} s |",
                     f"| Completion tokens | {k['per_node']['completion_tokens']} | {l['per_node']['completion_tokens']} |",
                     "| J/token (CPU+GPU) | " + " | ".join(
                         f"{x['joules_per_token']:.2f}" if x.get("joules_per_token") else
                         "not measured (powermetrics needs root)" for x in (k, l)) + " |"]),
                  ""]
    for key, v in r.items():
        if mode == "gpu" and isinstance(v, dict) and "joules_per_token" in v:
            lines += [f"## Run `{key}`", "", "```json", json.dumps(v, indent=2), "```", ""]
    text = "\n".join(lines)
    if path:
        Path(path).write_text(text)
    print(text)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--n", type=int, default=400, help="requests per overhead sample")
    common.add_argument("--sessions", type=int, default=16)
    common.add_argument("--turns", type=int, default=8)
    common.add_argument("--system-chars", type=int, default=6000)
    common.add_argument("--concurrency", type=int, default=4)
    common.add_argument("--out")
    lo = sub.add_parser("local", parents=[common])
    lo.add_argument("--bin-dir", default=str(ROOT / "target/release"))
    lo.add_argument("--cache-chunks", type=int, default=120)
    lo.add_argument("--ms-per-chunk", type=float, default=2.0)
    lo.set_defaults(fn=cmd_local)
    me = sub.add_parser("metal", parents=[common])
    me.add_argument("--bin-dir", default=str(ROOT / "target/release"))
    me.add_argument("--gguf", required=True)
    me.add_argument("--slots", type=int, default=4)
    me.add_argument("--ctx", type=int, default=32768)
    me.set_defaults(fn=cmd_metal)
    g = sub.add_parser("gpu", parents=[common])
    g.add_argument("--router", required=True, help="router base URL")
    g.add_argument("--engine", required=True, help="one engine's base URL, for direct overhead samples")
    g.add_argument("--metrics", required=True, help="router admin /metrics URL")
    g.add_argument("--model", required=True)
    g.add_argument("--api-key")
    g.add_argument("--label", default="kv_aware",
                   help="name this run; repeat with [router.score_weights] kv = 0 as load_only")
    g.set_defaults(fn=cmd_gpu)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
