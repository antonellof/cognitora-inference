#!/usr/bin/env python3
"""Bench client for OpenAI-compatible /v1/chat/completions endpoints.

Measurements per request:
  * TTFT  (streaming only) — time to first SSE chunk that contains a
          non-empty `choices[0].delta.content`. SSE preambles, role-only
          openers and empty heartbeats are explicitly *not* counted, so
          this number is comparable across direct-engine and proxied paths.
  * total_s — wallclock from POST until response body / SSE [DONE].
  * completion_tokens — from `usage.completion_tokens` when present, else
          from a streamed-chunk count (loose fallback).

Aggregates p50/p95/p99/mean across requests and writes one JSON record
per scenario to stdout.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass

import urllib.error
import urllib.request


@dataclass
class Sample:
    ok: bool
    ttft_s: float
    total_s: float
    completion_tokens: int
    prompt_tokens: int = 0
    error: str | None = None


def _http_post(url: str, payload: dict, *, stream: bool, timeout: float) -> Sample:
    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={
            "content-type": "application/json",
            "accept": "text/event-stream" if stream else "application/json",
        },
        method="POST",
    )
    t0 = time.perf_counter()
    try:
        resp = urllib.request.urlopen(req, timeout=timeout)
    except urllib.error.HTTPError as exc:
        return Sample(False, math.nan, time.perf_counter() - t0, 0, error=f"http {exc.code}: {exc.read()[:200]!r}")
    except Exception as exc:  # noqa: BLE001
        return Sample(False, math.nan, time.perf_counter() - t0, 0, error=f"err: {exc!r}")

    if not stream:
        raw = resp.read()
        total = time.perf_counter() - t0
        try:
            data = json.loads(raw)
            usage = data.get("usage", {}) or {}
            ct = int(usage.get("completion_tokens") or 0)
            pt = int(usage.get("prompt_tokens") or 0)
        except Exception:
            ct = pt = 0
        return Sample(True, total, total, ct, prompt_tokens=pt)

    ttft: float | None = None
    chunk_count = 0
    completion_tokens = 0
    prompt_tokens = 0
    try:
        for raw_line in resp:
            line = raw_line.decode("utf-8", errors="replace").strip()
            if not line or not line.startswith("data:"):
                continue
            payload_s = line[5:].strip()
            if payload_s == "[DONE]":
                break
            try:
                ev = json.loads(payload_s)
            except Exception:
                continue
            choices = ev.get("choices") or []
            usage = ev.get("usage") or {}
            ct = usage.get("completion_tokens")
            pt = usage.get("prompt_tokens")
            if isinstance(ct, int):
                completion_tokens = max(completion_tokens, ct)
            if isinstance(pt, int):
                prompt_tokens = max(prompt_tokens, pt)
            if not choices:
                continue
            delta = choices[0].get("delta") or {}
            content = delta.get("content") or ""
            if content == "":
                continue
            if ttft is None:
                ttft = time.perf_counter() - t0
            chunk_count += 1
        total = time.perf_counter() - t0
    except Exception as exc:  # noqa: BLE001
        return Sample(False, math.nan, time.perf_counter() - t0, 0, error=f"stream err: {exc!r}")

    if completion_tokens == 0:
        completion_tokens = chunk_count
    if ttft is None:
        ttft = total
    return Sample(True, ttft, total, completion_tokens, prompt_tokens=prompt_tokens)


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


def make_long_prompt(target_tokens: int, *, base_phrase: str | None = None) -> str:
    """Build a prompt of ~target_tokens tokens using a deterministic filler.

    Approximates 1 token ≈ 0.75 words; we just repeat the base phrase enough
    times to reach the target length. Engines tokenize this differently, but
    they tokenize *the same string* so the relative comparison stays valid.
    """
    base = base_phrase or (
        "The European Space Agency is preparing a new mission to map the surface of "
        "Mars at unprecedented resolution. The mission, code-named Iris, will use "
        "ground-penetrating radar to study subsurface ice, lava tubes, and ancient "
        "river beds, returning multi-spectral imagery to Earth via a relay satellite."
    )
    words_per_phrase = len(base.split())
    target_words = int(target_tokens * 0.75)
    repeats = max(1, target_words // words_per_phrase)
    body = (" " + base) * repeats
    return (
        "You are a precise technical assistant. Read the briefing below and then "
        "answer the question.\n\nBRIEFING:\n" + body.strip()
        + "\n\nQUESTION: In one short sentence, name the mission and its primary instrument."
    )


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
    print(
        f"[bench] {name}: n={len(prompts)} concurrency={concurrency} "
        f"stream={stream} model={model} url={url}",
        file=sys.stderr,
    )

    def one(p: str) -> Sample:
        body: dict = {
            "model": model,
            "messages": [{"role": "user", "content": p}],
            "max_tokens": max_tokens,
            "temperature": 0.0,
        }
        if stream:
            body["stream"] = True
            body["stream_options"] = {"include_usage": True}
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
    avg_prompt_tokens = (
        sum(s.prompt_tokens for s in ok) / n_ok if any(s.prompt_tokens for s in ok) else 0
    )

    return {
        "name": name,
        "url": url,
        "model": model,
        "stream": stream,
        "n": len(samples),
        "ok": n_ok,
        "err": len(samples) - n_ok,
        "wall_s": round(wall, 3),
        "avg_prompt_tokens": round(avg_prompt_tokens, 1),
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
    ap.add_argument("--prompt-tokens", type=int, default=0,
                    help="If > 0, generate a synthetic prompt of approximately this many tokens.")
    ap.add_argument("--shared-prefix", action="store_true",
                    help="Reuse the same prompt N times (engine prefix-cache demo).")
    ap.add_argument("--timeout", type=float, default=180.0)
    ap.add_argument("--warmup", type=int, default=2)
    args = ap.parse_args()

    if args.prompt_tokens > 0:
        base = [make_long_prompt(args.prompt_tokens)]
    else:
        base = [
            "Explain the difference between BFS and DFS in one short paragraph.",
            "Write a haiku about distributed systems.",
            "Summarize the role of an LLM router in two sentences.",
            "What is a key-value cache in transformer inference?",
            "Give one example of a CPU-friendly model.",
            "Why is TTFT important for chat UX?",
        ]

    if args.shared_prefix:
        prompts = [base[0]] * args.n
    else:
        prompts = [base[i % len(base)] for i in range(args.n)]

    if args.warmup > 0:
        warm_prompts = prompts[: max(1, min(args.warmup, len(prompts)))]
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
