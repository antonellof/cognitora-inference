#!/usr/bin/env python3
"""Generate the deterministic prompt workload for the disagg benchmark.

Emits JSONL (one {"prompt": ..., "kind": "shared"|"unique"} per line)
with a fixed mix of:

  * shared-prefix prompts — a common long briefing prefix (~PREFIX_TOKENS
    tokens) followed by a short per-request question. These are the
    requests where KV prefix reuse / disaggregated prefill pays off.
  * unique prompts — fully distinct texts, so the engine gets no prefix
    reuse. These keep the comparison honest.

Everything is seeded, so `--seed 0 --n 32` always produces byte-identical
output: both bench modes (disagg, agg) replay the exact same workload.

Usage:
  workload.py --n 32 --out workload.jsonl \
      [--shared-frac 0.6] [--prefix-tokens 512] [--seed 0]
"""
from __future__ import annotations

import argparse
import json
import random
import sys

BRIEFING = (
    "The European Space Agency is preparing a new mission to map the surface of "
    "Mars at unprecedented resolution. The mission, code-named Iris, will use "
    "ground-penetrating radar to study subsurface ice, lava tubes, and ancient "
    "river beds, returning multi-spectral imagery to Earth via a relay satellite."
)

TOPICS = [
    "consensus protocols in distributed databases",
    "the history of container orchestration",
    "GPU memory hierarchies and tensor parallelism",
    "speculative decoding for language models",
    "the economics of spot-instance clusters",
    "quantization formats for on-device inference",
    "content-addressable storage systems",
    "network topologies for RDMA fabrics",
    "scheduling theory and Smith's rule",
    "the design of write-ahead logs",
    "erasure coding in object stores",
    "cache coherency in NUMA systems",
]

QUESTIONS = [
    "name the mission and its primary instrument",
    "list two science goals in one sentence",
    "explain how the data reaches Earth",
    "state which planetary features are studied",
    "describe the radar technique in one sentence",
    "summarize the briefing in ten words",
]


def shared_prefix(prefix_tokens: int) -> str:
    """Repeat the briefing until ~prefix_tokens tokens (1 tok ≈ 0.75 words)."""
    words_per = len(BRIEFING.split())
    target_words = max(words_per, int(max(prefix_tokens, 40) * 0.75))
    repeats = max(1, target_words // words_per)
    body = " ".join([BRIEFING] * repeats)
    return (
        "You are a precise technical assistant. Read the briefing below and "
        "then answer the question.\n\nBRIEFING:\n" + body
    )


def unique_prompt(rng: random.Random, i: int) -> str:
    topic = rng.choice(TOPICS)
    words = rng.randint(20, 60)
    return (
        f"Request {i}: write approximately {words} words explaining {topic} "
        f"to a colleague, using the number {rng.randint(1000, 9999)} as a "
        "worked example somewhere in the text."
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--shared-frac", type=float, default=0.6,
                    help="Fraction of prompts sharing the common prefix (default 0.6).")
    ap.add_argument("--prefix-tokens", type=int, default=512,
                    help="Approximate token length of the shared prefix (default 512).")
    ap.add_argument("--seed", type=int, default=0)
    args = ap.parse_args()

    rng = random.Random(args.seed)
    prefix = shared_prefix(args.prefix_tokens)

    n_shared = 0
    with open(args.out, "w", encoding="utf-8") as f:
        acc = 0.0
        for i in range(args.n):
            # Deterministic interleave: shared and unique prompts are mixed
            # throughout the run instead of front-loaded, so concurrency
            # windows always contain both kinds.
            acc += args.shared_frac
            if acc >= 1.0:
                acc -= 1.0
                q = QUESTIONS[i % len(QUESTIONS)]
                prompt = f"{prefix}\n\nQUESTION {i}: In one short sentence, {q}."
                kind = "shared"
                n_shared += 1
            else:
                prompt = unique_prompt(rng, i)
                kind = "unique"
            f.write(json.dumps({"prompt": prompt, "kind": kind}) + "\n")

    print(
        f"[workload] wrote {args.n} prompts to {args.out} "
        f"({n_shared} shared-prefix @ ~{args.prefix_tokens} tokens, "
        f"{args.n - n_shared} unique, seed={args.seed})",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
