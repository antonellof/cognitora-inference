#!/usr/bin/env python3
"""Single-host bench client for OpenAI-compatible /v1/chat/completions endpoints.

Measures, per request:
  * TTFT  -- time from POST to first SSE token (streaming) or to response start
            (non-streaming; reported as full latency).
  * Total wall latency.
  * Generated token count (from `usage.completion_tokens` if available, else
    streamed-chunk count).
  * Tokens/sec (decode throughput) using completion_tokens / (total - TTFT).

Aggregates p50/p95/p99 across requests and writes a JSON record to stdout.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import asdict, dataclass

import urllib.request
import urllib.error


@dataclass
class Sample:
    ok: bool
    ttft_s: float
    total_s: float
    completion_tokens: int
    error: str | None = None


def _http_post(url: str, payload: dict, stream: bool, timeout: float) -> Sample:
    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={"content-type": "application/json", "accept": "text/event-stream" if stream else "application/json"},
        method="POST",
    )
    t0 = time.perf_counter()
    try:
        resp = urllib.request.urlopen(req, timeout=timeout)
    except urllib.error.HTTPError as exc:
        return Sample(False, math.nan, time.perf_counter() - t0, 0, f"http {exc.code}: {exc.read()[:200]!r}")
    except Exception as exc:  # noqa: BLE001
        return Sample(False, math.nan, time.perf_counter() - t0, 0, f"err: {exc!r}")

    if not stream:
        raw = resp.read()
        total = time.perf_counter() - t0
        try:
            data = json.loads(raw)
            ct = int(data.get("usage", {}).get("completion_tokens") or 0)
        except Exception:
            ct = 0
        return Sample(True, total, total, ct)

    ttft: float | None = None
    chunk_count = 0
    completion_tokens = 0
    try:
        for line in resp:
            line = line.decode("utf-8", errors="replace").strip()
            if not line or not line.startswith("data:"):
                continue
            payload = line[5:].strip()
            if payload == "[DONE]":
                break
            if ttft is None:
                ttft = time.perf_counter() - t0
            chunk_count += 1
            try:
                ev = json.loads(payload)
                u = ev.get("usage") or {}
                ct = u.get("completion_tokens")
                if isinstance(ct, int):
                    completion_tokens = max(completion_tokens, ct)
            except Exception:
                pass
        total = time.perf_counter() - t0
    except Exception as exc:  # noqa: BLE001
        return Sample(False, math.nan, time.perf_counter() - t0, 0, f"stream err: {exc!r}")

    if completion_tokens == 0:
        completion_tokens = chunk_count
    if ttft is None:
        ttft = total
    return Sample(True, ttft, total, completion_tokens)


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return math.nan
    s = sorted(values)
    k = (len(s) - 1) * pct
    lo = math.floor(k)
    hi = math.ceil(k)
    if lo == hi:
        return s[int(k)]
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


def run_scenario(
    name: str,
    url: str,
    model: str,
    prompts: list[str],
    *,
    stream: bool,
    max_tokens: int,
    concurrency: int,
    timeout: float,
) -> dict:
    print(f"[bench] {name}: n={len(prompts)} concurrency={concurrency} stream={stream} model={model} url={url}", file=sys.stderr)

    def one(p: str) -> Sample:
        body = {
            "model": model,
            "messages": [{"role": "user", "content": p}],
            "max_tokens": max_tokens,
            "temperature": 0.0,
        }
        if stream:
            body["stream"] = True
        return _http_post(url, body, stream=stream, timeout=timeout)

    samples: list[Sample] = []
    t0 = time.perf_counter()
    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        futs = [pool.submit(one, p) for p in prompts]
        for fut in as_completed(futs):
            samples.append(fut.result())
    wall = time.perf_counter() - t0

    ok = [s for s in samples if s.ok]
    n_ok = len(ok)
    n_err = len(samples) - n_ok
    if n_ok == 0:
        return {
            "name": name,
            "n": len(samples),
            "ok": 0,
            "errors": [s.error for s in samples][:3],
            "wall_s": wall,
        }

    ttfts = [s.ttft_s for s in ok]
    totals = [s.total_s for s in ok]
    decodes_s: list[float] = []
    for s in ok:
        decode = s.total_s - s.ttft_s if stream else s.total_s
        if s.completion_tokens > 0 and decode > 0:
            decodes_s.append(s.completion_tokens / decode)
    total_completion = sum(s.completion_tokens for s in ok)

    return {
        "name": name,
        "url": url,
        "model": model,
        "stream": stream,
        "n": len(samples),
        "ok": n_ok,
        "err": n_err,
        "wall_s": round(wall, 3),
        "ttft_ms": {
            "p50": round(percentile(ttfts, 0.5) * 1000, 1),
            "p95": round(percentile(ttfts, 0.95) * 1000, 1),
            "p99": round(percentile(ttfts, 0.99) * 1000, 1),
            "mean": round(statistics.fmean(ttfts) * 1000, 1),
        },
        "total_ms": {
            "p50": round(percentile(totals, 0.5) * 1000, 1),
            "p95": round(percentile(totals, 0.95) * 1000, 1),
            "p99": round(percentile(totals, 0.99) * 1000, 1),
            "mean": round(statistics.fmean(totals) * 1000, 1),
        },
        "decode_tps": {
            "p50": round(percentile(decodes_s, 0.5), 2) if decodes_s else None,
            "mean": round(statistics.fmean(decodes_s), 2) if decodes_s else None,
        },
        "total_completion_tokens": total_completion,
        "system_tps": round(total_completion / wall, 2) if wall > 0 else None,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--name", required=True)
    ap.add_argument("--url", required=True, help="Full /v1/chat/completions URL")
    ap.add_argument("--model", required=True)
    ap.add_argument("--n", type=int, default=20)
    ap.add_argument("--concurrency", type=int, default=1)
    ap.add_argument("--max-tokens", type=int, default=64)
    ap.add_argument("--stream", action="store_true")
    ap.add_argument("--prompt-file", default=None, help="One prompt per line; cycled")
    ap.add_argument("--shared-prefix", action="store_true", help="Reuse the same prompt N times (KV-reuse mode)")
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument("--warmup", type=int, default=2)
    args = ap.parse_args()

    if args.prompt_file:
        with open(args.prompt_file) as fh:
            base = [ln.strip() for ln in fh if ln.strip()]
    else:
        base = [
            "Explain the difference between BFS and DFS in one short paragraph.",
            "Write a haiku about distributed systems.",
            "Summarize the role of an LLM router in two sentences.",
            "What is a key-value cache in transformer inference?",
            "Give one example of a CPU-friendly model.",
            "Why is TTFT important for chat UX?",
        ]
    if not base:
        print("no prompts", file=sys.stderr)
        return 2

    if args.shared_prefix:
        prompts = [base[0]] * args.n
    else:
        prompts = [base[i % len(base)] for i in range(args.n)]

    if args.warmup > 0:
        warm_prompts = prompts[: args.warmup]
        run_scenario(
            args.name + "/warmup",
            args.url,
            args.model,
            warm_prompts,
            stream=args.stream,
            max_tokens=min(args.max_tokens, 32),
            concurrency=1,
            timeout=args.timeout,
        )

    res = run_scenario(
        args.name,
        args.url,
        args.model,
        prompts,
        stream=args.stream,
        max_tokens=args.max_tokens,
        concurrency=args.concurrency,
        timeout=args.timeout,
    )
    json.dump(res, sys.stdout)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
