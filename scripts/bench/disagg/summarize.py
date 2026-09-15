#!/usr/bin/env python3
"""Summarize disaggregation bench results.

Reads the JSONL emitted by scripts/bench/bench_client.py (one record per
scenario, named "disagg" and/or "agg"), writes:

  * a combined results.json (records + run metadata + host info), and
  * a markdown summary table with TTFT p50/p95, decode tok/s and system
    tok/s per mode, plus disagg-vs-agg deltas when both modes ran.

All numbers come verbatim from the bench client's measurements — this
script never synthesizes values.
"""
from __future__ import annotations

import argparse
import datetime
import json
import platform
import sys


def fmt(v, suffix=""):
    if v is None:
        return "n/a"
    return f"{v}{suffix}"


def delta_pct(disagg: float | None, agg: float | None) -> str:
    """Signed percentage change of disagg relative to agg."""
    if disagg is None or agg is None or agg == 0:
        return "n/a"
    return f"{(disagg - agg) / agg * 100:+.1f}%"


def completion_tokens_invalid(r: dict) -> bool:
    """True when a scenario has no real completion tokens."""
    if "total_completion_tokens" in r:
        return r.get("total_completion_tokens", 0) == 0
    return r.get("ok", 0) == 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp", required=True)
    ap.add_argument("--out-json", required=True)
    ap.add_argument("--out-md", required=True)
    ap.add_argument("--meta", nargs="*", default=[], help="key=value run metadata")
    args = ap.parse_args()

    records = []
    with open(args.inp, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                records.append(json.loads(line))
    # Keep only final scenarios (bench_client prints warmups to stderr,
    # but be defensive in case of future changes).
    by_mode = {r["name"]: r for r in records if not r["name"].endswith("/warmup")}

    meta = dict(kv.split("=", 1) for kv in args.meta if "=" in kv)
    combined = {
        "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "host": platform.node(),
        "platform": platform.platform(),
        "meta": meta,
        "scenarios": records,
    }
    with open(args.out_json, "w", encoding="utf-8") as f:
        json.dump(combined, f, indent=2)

    lines = [
        "# Disaggregation benchmark summary",
        "",
        f"Model: `{meta.get('model', '?')}` · n={meta.get('n', '?')} · "
        f"concurrency={meta.get('concurrency', '?')} · "
        f"max_tokens={meta.get('max_tokens', '?')} · "
        f"prompt_tokens={meta.get('prompt_tokens', '?')}",
        "",
    ]
    invalid = [
        mode
        for mode in ("disagg", "agg")
        if (r := by_mode.get(mode)) and completion_tokens_invalid(r)
    ]
    if invalid:
        modes_str = ", ".join(f"`{m}`" for m in invalid)
        lines += [
            f"> **Warning:** Zero completion tokens for {modes_str}.",
            "> TTFT and tok/s numbers are not from real inference — engine is likely down or misconfigured.",
            "",
        ]
    lines += [
        "| mode | ok/n | TTFT p50 (ms) | TTFT p95 (ms) | decode tok/s (p50) | system tok/s |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for mode in ("disagg", "agg"):
        r = by_mode.get(mode)
        if r is None:
            continue
        if r.get("ok", 0) == 0:
            lines.append(f"| {mode} | 0/{r.get('n', '?')} | failed | failed | failed | failed |")
            continue
        ttft = r.get("ttft_ms", {})
        tps = r.get("decode_tps", {})
        lines.append(
            f"| {mode} | {r['ok']}/{r['n']} "
            f"| {fmt(ttft.get('p50'))} | {fmt(ttft.get('p95'))} "
            f"| {fmt(tps.get('p50'))} | {fmt(r.get('system_tps'))} |"
        )

    d, a = by_mode.get("disagg"), by_mode.get("agg")
    if d and a and d.get("ok") and a.get("ok"):
        lines += [
            "",
            "## disagg vs agg (positive = disagg higher)",
            "",
            "| metric | disagg | agg | delta |",
            "|---|---:|---:|---:|",
            f"| TTFT p50 (ms) | {d['ttft_ms']['p50']} | {a['ttft_ms']['p50']} "
            f"| {delta_pct(d['ttft_ms']['p50'], a['ttft_ms']['p50'])} |",
            f"| TTFT p95 (ms) | {d['ttft_ms']['p95']} | {a['ttft_ms']['p95']} "
            f"| {delta_pct(d['ttft_ms']['p95'], a['ttft_ms']['p95'])} |",
            f"| system tok/s | {fmt(d.get('system_tps'))} | {fmt(a.get('system_tps'))} "
            f"| {delta_pct(d.get('system_tps'), a.get('system_tps'))} |",
        ]

    with open(args.out_md, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
