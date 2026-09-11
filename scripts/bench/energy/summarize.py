#!/usr/bin/env python3
"""Write energy benchmark summary markdown from before/after/bench JSON."""
from __future__ import annotations

import argparse
import datetime
import json
import platform


def joules_per_token(avg_watts: float, wall_s: float, tokens: int) -> float | None:
    if tokens <= 0 or avg_watts <= 0:
        return None
    return (avg_watts * wall_s) / tokens


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--before", required=True)
    ap.add_argument("--after", required=True)
    ap.add_argument("--bench", required=True)
    ap.add_argument("--out-md", required=True)
    ap.add_argument("--out-json", required=True)
    ap.add_argument("--meta", nargs="*", default=[])
    args = ap.parse_args()

    before = json.load(open(args.before, encoding="utf-8"))
    after = json.load(open(args.after, encoding="utf-8"))
    bench = json.load(open(args.bench, encoding="utf-8"))

    meta = dict(kv.split("=", 1) for kv in args.meta if "=" in kv)
    dt = max(after.get("t", 0) - before.get("t", 0), 0.001)
    tok_delta = max(
        after.get("completion_tokens", 0) - before.get("completion_tokens", 0),
        bench.get("total_completion_tokens", 0),
    )
    avg_w = (before.get("power_watts", 0) + after.get("power_watts", 0)) / 2
    wall = bench.get("wall_s", dt)
    tps = tok_delta / wall if wall > 0 else 0
    tpw = tps / avg_w if avg_w > 0 else None
    jpt = joules_per_token(avg_w, wall, int(tok_delta))

    out = {
        "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "host": platform.node(),
        "platform": platform.platform(),
        "meta": meta,
        "before": before,
        "after": after,
        "bench": bench,
        "energy": {
            "avg_power_watts": avg_w,
            "completion_tokens": tok_delta,
            "wall_s": wall,
            "tokens_per_s": tps,
            "tokens_per_watt": tpw,
            "joules_per_token": jpt,
        },
    }
    json.dump(out, open(args.out_json, "w", encoding="utf-8"), indent=2)

    lines = [
        "# Energy benchmark summary",
        "",
        f"Model: `{meta.get('model', '?')}` · n={meta.get('n', '?')} · "
        f"concurrency={meta.get('concurrency', '?')}",
        "",
        "| metric | value |",
        "|---|---:|",
        f"| avg fleet power (W) | {avg_w:.1f} |",
        f"| completion tokens | {int(tok_delta)} |",
        f"| wall time (s) | {wall:.2f} |",
        f"| tokens/s | {tps:.2f} |",
        f"| tokens/s per W | {tpw:.4f} |" if tpw is not None else "| tokens/s per W | n/a |",
        f"| J/token | {jpt:.2f} |" if jpt is not None else "| J/token | n/a |",
        "",
        "_Numbers are derived from router `/metrics` power gauges and "
        "`cgn_router_chat_completion_tokens_total` plus bench_client wall time._",
        "",
        "## Published results",
        "",
        "| date | hardware | tokens/s per W | J/token |",
        "|---|---|---:|---:|",
        "| — | — | — | — |",
    ]
    open(args.out_md, "w", encoding="utf-8").write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
