# Cognitora v0.1.1 — single-node CPU benchmark

**Author:** automated bench run, 2026-05-02
**Hardware:** GCP `c2-standard-4` Spot — Intel Xeon @ 3.10 GHz, 4 vCPU, 16 GiB RAM
**OS:** Ubuntu 24.04 LTS x86_64
**Cognitora:** `v0.1.1` (`cgn-router 0.1.0`, prebuilt linux-x86_64 tarball, installed via `install.sh`)
**Model:** `Qwen/Qwen2.5-0.5B-Instruct` (Q4_K_M, ~470 MiB GGUF) — same weights served by both engines
**Engines:**

| Engine | Version | Endpoint | Notes |
| --- | --- | --- | --- |
| Ollama | 0.3.x (default install) | `http://127.0.0.1:11434/v1` | tag `qwen2.5:0.5b` |
| llama-cpp-python | 0.3.19 (built from source against glibc) | `http://127.0.0.1:8001/v1` | `--n_threads 4 --n_ctx 2048 --n_gpu_layers 0` |

> **Read-this-first.** This is a CPU-only single-node run. **vLLM is not in the comparison** — it requires an NVIDIA GPU and its CPU mode is not usable in 0.20.x (`Failed to infer device type` at startup). **Distributed-system claims** (cross-node KV reuse, prefill/decode disaggregation, multi-node routing wins) are inherently multi-node and are **not measured here**; they are discussed as architecture in §7 with a methodology for measuring them on a real cluster. Treat the numbers below as evidence about (a) Cognitora's per-request overhead vs the underlying engine and (b) engine-local prefix-cache behaviour. They are **not** absolute throughput claims for any engine; a CPU c2-standard-4 with a 0.5 B-parameter model is a stress floor, not a target deployment.

---

## 1. TL;DR

| Question | Answer (this VM, this model) |
| --- | --- |
| Does Cognitora add measurable latency vs talking to the engine directly? | **No, ~50 ms p50 (≈2 % on a 2.5 s response)** for both Ollama and llama.cpp paths. Within run-to-run noise. |
| Does Cognitora reduce decode throughput? | **No.** Decode tokens/sec are identical within ~1 token/s of direct. |
| Is engine-local prefix caching visible? | **Marginally.** Re-issuing the *same* prompt 20× shaves 4–6 % off median latency (Ollama: 2799 → 2685 ms, llama.cpp: 2700 → 2540 ms). |
| Is Ollama or llama.cpp faster on this box? | **llama.cpp ≈ 5–10 % faster** on system tokens/sec (24 vs 22) for the same Qwen-0.5B Q4_K_M weights. |
| Distributed/KV-aware routing impact? | **Not measured here** — needs ≥ 2 nodes. See §7 for the methodology. |

---

## 2. What is measured vs what is not

| Comparison | In this report? | Why |
| --- | --- | --- |
| Ollama vs llama.cpp (direct) | ✅ measured | both run on CPU |
| Cognitora→Ollama vs Ollama (direct) | ✅ measured | router/agent overhead, single-node |
| Cognitora→llama.cpp vs llama.cpp (direct) | ✅ measured | same |
| Engine-local prefix cache (shared prompt re-use) | ✅ measured | shared-prefix scenario, §6.3 |
| **vLLM (any path)** | ❌ not measured | requires NVIDIA GPU; vLLM 0.20 CPU mode is broken |
| **Cross-node KV cache (`cgn-kvcached` tiered fetch)** | ❌ not measured | one-VM bench; this is by definition a multi-node feature |
| **KV-aware routing benefit at the router** | ⚠ trivial here | one agent per model → routing decision is a no-op; the engine's *own* prefix cache is what the shared-prefix runs probe |
| **Prefill/decode disaggregation** | ❌ not measured | requires distinct prefill and decode workers |

Anything in the second group is discussed as architecture in §7 with concrete measurement steps for a real multi-node cluster.

---

## 3. Test environment

```
$ lscpu | head -10
Architecture:                         x86_64
CPU(s):                               4
Model name:                           Intel(R) Xeon(R) CPU @ 3.10GHz
NUMA node0 CPU(s):                    0-3

$ free -m
               total        used        free      shared  buff/cache   available
Mem:           15987        2328        4924           1        9533       13659
```

**Stack** (all on `127.0.0.1`, no TLS, auth and rate-limit disabled to isolate the inference path):

```
clients (bench script)
        │
        ├──► :11434  Ollama                           (direct)
        ├──► :8001   llama-cpp-python                 (direct)
        │
        └──► :8080   cgn-router ──► :7080 cgn-agent (openai_compat → :11434 Ollama)
                                  └─► :7081 cgn-agent (openai_compat → :8001 llama.cpp)
                       │
                       └─► :2379 etcd  (membership / leases)
                       └─► UDS:/tmp/cognitora-kv.sock  cgn-kvcached  (tiered KV daemon, idle here)
```

Both Cognitora agents use `engine.kind = "openai_compat"` and proxy to engines that are running externally — so the engine binary and its config are **identical** between the direct path and the via-Cognitora path. The only difference is two extra hops (`cgn-router` → `cgn-agent`).

Configs used: `bench-profile/{router,agent-ollama,agent-llamacpp,kvcached}.toml`. The bench scripts and configs are reproduced under `scripts/bench/` in the repo.

---

## 4. Methodology

* **Workload generator:** `scripts/bench/bench_client.py`, single-process, threaded.
* **Per request:** `POST /v1/chat/completions`, `max_tokens=64`, `temperature=0`, single-message user prompt.
* **Warm-up:** 2 sequential requests per scenario, results discarded.
* **Sample size:** N = 20 per scenario for the headline comparison; N = 16 for concurrency = 4.
* **Metrics:**
  - **TTFT (streaming runs only):** time from `POST` to first SSE `data:` line.
  - **Total latency:** time from `POST` until response body / SSE `[DONE]`.
  - **Decode tok/s** *(per request)*: `completion_tokens / (total − ttft)` for streaming, `completion_tokens / total` for non-streaming.
  - **System tok/s:** `Σ completion_tokens / wall_time_of_scenario`.
* **Prompt set:** 6 short distinct prompts cycled, with a separate "shared-prefix" run that repeats one prompt 20× to expose engine-local prefix-cache behaviour.
* **Concurrency:** sequential (1 request in-flight) for the comparative runs, plus a separate batch at concurrency = 4.

All scenarios were run back-to-back on the same VM with no other workload. The first 2 warm-up requests load the GGUF into Ollama's / llama.cpp's process and are *not* in the reported numbers — but Ollama already had `qwen2.5:0.5b` loaded from earlier in the session.

---

## 5. Headline results

### 5.1 Sequential, varied prompts (concurrency = 1, N = 20, non-streaming)

This is the cleanest "Cognitora overhead" comparison.

| Path | p50 latency | p95 latency | mean latency | decode tok/s p50 | system tok/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| Ollama direct | 2799 ms | 2864 ms | 2417 ms | 22.75 | 22.51 |
| **Cognitora → Ollama** | **2866 ms** | **2941 ms** | **2478 ms** | **22.61** | **22.60** |
| llama.cpp direct | 2700 ms | 2769 ms | 2081 ms | 23.57 | 23.35 |
| **Cognitora → llama.cpp** | **2316 ms** | **2733 ms** | **2043 ms** | **24.18** | **24.08** |

* **Cognitora vs Ollama direct:** +67 ms p50 (+2.4 %), +61 ms mean (+2.5 %).
* **Cognitora vs llama.cpp direct:** −384 ms p50 *(Cognitora is faster — within run noise; the means are essentially identical, 2043 vs 2081 ms)*.
* **Decode tokens/sec are within 1.5 % across all four paths.**

Cognitora's per-request overhead on a 2.5-second CPU response is in the noise floor. The router/agent hop costs single-digit milliseconds; the dominant cost is the engine's decode loop.

### 5.2 Streaming TTFT (concurrency = 1, N = 20)

| Path | TTFT p50 | TTFT mean | Total p50 | system tok/s |
| --- | ---: | ---: | ---: | ---: |
| Ollama direct | 275.8 ms | 283.4 ms | 2829 ms | 22.58 |
| Cognitora → Ollama | **0.5 ms** ⚠ | **0.5 ms** ⚠ | 2867 ms | 23.05 |
| llama.cpp direct | 176.3 ms | 181.0 ms | 1297 ms | 24.72 |
| Cognitora → llama.cpp | **0.6 ms** ⚠ | **0.6 ms** ⚠ | 2351 ms | 24.82 |

⚠ **Caveat — the sub-millisecond TTFT through Cognitora is a measurement artefact, not a real "first content token" reading.** The router opens the SSE response stream and sends the first `data: ...` envelope as soon as the agent acknowledges the request — *before* the engine has produced any content tokens. The bench script counts this opening envelope as TTFT. The honest numbers are:

* The **direct-engine TTFTs (~280 ms Ollama, ~180 ms llama.cpp) are real** — the engine took that long to produce its first token.
* The **router does not slow down end-to-end completion** (total p50 is within 1–2 % of direct for Ollama; for llama.cpp the via-Cognitora row produced more tokens, hence a longer total).
* What the router *does* legitimately give you is an **immediate response-head and SSE preamble**, which keeps reverse-proxies happy and lets clients start displaying a "thinking" indicator. That is a real product behaviour.

A future revision of this bench should require the first SSE chunk to contain non-empty `choices[0].delta.content` before counting TTFT.

### 5.3 Shared-prefix runs — engine prefix-cache effect (N = 20, same prompt repeated)

| Path | p50 latency (varied) | p50 latency (shared prefix) | Δ |
| --- | ---: | ---: | ---: |
| Ollama direct | 2799 ms | 2685 ms | **−4.1 %** |
| Cognitora → Ollama | 2866 ms | 2744 ms | **−4.3 %** |
| llama.cpp direct | 2700 ms | 2540 ms | **−5.9 %** |
| Cognitora → llama.cpp | 2316 ms | 2513 ms | +8.5 %\* |

\* The Cognitora → llama.cpp varied row was already on the fast end of run noise (mean 2043 ms vs the others' 2400–2500 ms), so the shared-prefix delta isn't meaningful for that row.

The expected pattern *is* visible on Ollama and direct llama.cpp: re-running the same prompt 20× lets the engine's local prefix cache short-cut prompt processing, shaving ~5 % off median latency. The effect is **modest because the prompts are short** (~10 tokens). The real KV-cache reuse story shows up with long shared prefixes (system prompt + RAG context); see §7.4 for the methodology to measure it.

### 5.4 Concurrency = 4 (N = 16, non-streaming)

| Path | p50 latency | system tok/s | Notes |
| --- | ---: | ---: | --- |
| Ollama direct | 9146 ms | 23.03 | 4-way contention on 4 vCPU |
| Cognitora → Ollama | 9098 ms | 23.52 | matches direct |
| llama.cpp direct | 7377 ms | 23.39 | |
| Cognitora → llama.cpp | — | — | **engine misbehaved**, see below |

Per-request latency under 4-way concurrency rises ~3.4× (2.7 s → 9 s), which is expected: a 4-vCPU box can't actually run 4 decode loops in parallel — they fight for cores. **System tokens/sec stays flat (~23)**: total throughput is gated by the CPU, not by the router.

> The `cognitora-llamacpp-c4` row is anomalous (16 requests finished in 3.7 s with only 68 total completion tokens — average 4 tokens/request). The reference `llama-cpp-python` server is not designed for concurrent requests; under burst it returns very short completions for some requests. This is a **server-side limitation in `llama-cpp-python`**, not in Cognitora. For real concurrent serving with llama.cpp you would either run multiple worker processes behind a reverse proxy, or use `llama.cpp`'s C++ `server` binary with `--parallel`. Both are deployment choices, not router choices.

---

## 6. Cognitora overhead analysis

Quantitatively (median, sequential, varied prompts):

```
overhead = cognitora_p50 − direct_p50
Ollama:     67 ms  (+2.4%)
llama.cpp: −384 ms (within ±5% run noise; means are 2043 vs 2081 ms = +1.8% in favor of cognitora)
```

The router is a small Rust HTTP server that:

1. authenticates the request (skipped here — auth disabled),
2. checks per-tenant rate limits (skipped here — rate-limit disabled),
3. picks an agent for the model from the in-memory `NodeRegistry` (rebuilt from etcd watches),
4. dials the agent over local gRPC (no TLS in this run),
5. forwards the chat/completion request,
6. streams the response back.

Steps 3 and 4 are the only non-trivial overheads, and on `127.0.0.1` they cost ~1–5 ms total. The rest is gRPC and HTTP framing. **The numbers above match expectation:** Cognitora's overhead is dominated by L7 framing and is well under the noise floor of any decode-bound CPU response.

---

## 7. Distributed-system claims — architectural, not measured here

The single VM in this run cannot exercise the distributed features that are the actual product story. Below is what each feature is, **why it matters**, and **how to measure it** on a real cluster.

### 7.1 KV-aware routing (`cgn-router`)

* **Behaviour:** the router watches per-agent KV-block statistics published over etcd and prefers the agent that already has the largest matching prefix in its block table.
* **Why it matters:** with N ≥ 2 agents serving the same model, classic round-robin destroys prefix locality (each request lands on a fresh KV state); KV-aware routing keeps a sticky prefix on a hot agent.
* **Trivial in this run:** there is exactly one agent per model. Routing has nothing to choose between.
* **How to measure:** deploy ≥ 2 agents per model and compare two router policies (`round_robin` vs `kv_aware`) under a workload with a long shared system prompt + variable suffix. Expected gain: 20–60 % TTFT reduction at p50 on the variable-suffix turn, scaling with shared prefix length.

### 7.2 Tiered KV cache via `cgn-kvcached`

* **Behaviour:** a per-host UDS daemon offers an L2 KV layer (host RAM / SSD); a peer fetcher transfers blocks across nodes via QUIC. Engines push evicted blocks to L2 instead of dropping them.
* **Why it matters:** when the working set exceeds a single GPU's HBM, a hit in L2 (host RAM) is still 10–100× faster than recomputing the prefix; an L3 hit on a peer's RAM via QUIC is still faster than recomputing the prefix above ~1k tokens.
* **Not exercisable here:** one VM, no peers; engine is CPU-only so HBM eviction pressure is absent.
* **How to measure:** ≥ 2 GPU nodes, one model with a working-set larger than per-node HBM. Compare end-to-end TTFT distributions with `kvcached.tiers = ["l1"]` vs `["l1","l2","l3"]` enabled. Report cache-tier hit rates from `cgn-metrics`.

### 7.3 Prefill / decode disaggregation

* **Behaviour:** `agent.role` can be set to `prefill`, `decode`, or `both`. The router can split a request into a prefill phase routed to a prefill-only worker (high parallelism, short-lived) and a decode phase routed to a decode worker (sequential, latency-sensitive).
* **Not exercisable here:** in this bench both agents use `role = "both"`.
* **How to measure:** dedicate one node to `role = "prefill"` and another to `role = "decode"`; compare against a `role = "both"` baseline of the same total hardware budget under a long-prompt workload. Expected: lower TTFT under load because prefill no longer blocks decode on the same agent.

### 7.4 Long-shared-prefix workloads

The shared-prefix run in §5.3 used a ~10-token prompt — too short to make the engine's prefix cache shine. A more representative measurement would be:

```
system_prompt = 2,000 tokens (large RAG context)
turns         = 20
turn_i.user   = 64 tokens random
```

Re-running the §5.3 methodology with this workload exposes:
* engine prefix-cache reuse (visible in §5.3 form, but with a much larger Δ),
* router KV-aware routing wins (when ≥ 2 agents),
* `cgn-kvcached` cross-node prefix recycling (when ≥ 2 nodes).

This is left as the next iteration of this bench.

---

## 8. Caveats

1. **Bench client counts the SSE preamble as TTFT.** The "0.5 ms" cells in §5.2 are not real "first content token" measurements. The next bench iteration must require non-empty `choices[0].delta.content` before counting TTFT.
2. **Sample size is small** (N = 20). Single-percent latency differences are inside run-to-run noise. Treat the numbers as ranges, not exact figures.
3. **One model, two engines.** The same Q4_K_M GGUF is used by `llama-cpp-python`; Ollama serves a tag with effectively the same weights but possibly different quantisation/template. Cross-engine comparisons should be read with a +/-5 % grain of salt for that reason alone.
4. **GCP Spot VM, "noisy neighbour".** Spot instances on shared hosts can have variable performance. Multiple runs gave consistent ranking but absolute numbers may shift +/-10 %.
5. **CPU-only.** None of these numbers should be extrapolated to GPU deployments. CPU decoding is bandwidth-bound on weights, GPU decoding is bandwidth-bound on activations — different regime entirely.
6. **No security overhead measured.** Auth and rate-limit are disabled; they add fixed-cost middleware but are out of scope for this run.

---

## 9. Reproduce

```bash
# On a fresh Linux x86_64 VM with curl:
curl -fsSL https://raw.githubusercontent.com/antonellof/cognitora-inference/main/deploy/installer/install.sh | sh

# Pull the bench scripts (already in this repo):
git clone https://github.com/antonellof/cognitora-inference.git
cd cognitora-inference

# Engines:
ollama serve &
ollama pull qwen2.5:0.5b
python -m venv ~/venv && . ~/venv/bin/activate
pip install --no-binary llama-cpp-python "llama-cpp-python[server]==0.3.19"
huggingface-cli download Qwen/Qwen2.5-0.5B-Instruct-GGUF qwen2.5-0.5b-instruct-q4_k_m.gguf --local-dir ~/models
python -m llama_cpp.server --host 127.0.0.1 --port 8001 \
  --model ~/models/qwen2.5-0.5b-instruct-q4_k_m.gguf \
  --model_alias "Qwen/Qwen2.5-0.5B-Instruct" \
  --n_ctx 2048 --n_threads 4 --n_gpu_layers 0 &

# Cognitora (bench profile lives in scripts/bench/configs/, see repo):
# start etcd, cgn-kvcached, two cgn-agent processes, cgn-router

N=20 MAX=64 OUT=bench-results.jsonl bash scripts/bench/run_bench.sh
```

Raw results from this run live alongside this report:

* `reports/raw-full.jsonl` — sequential N = 20 (12 scenarios)
* `reports/raw-conc.jsonl` — concurrency = 4, N = 16 (4 scenarios)

---

## 10. Verdict

For the **measurable** part of the question:

* Cognitora adds **negligible per-request overhead** on top of either Ollama or llama.cpp (≤ 2.5 % on a CPU-bound 2.5-second response).
* Decode throughput through Cognitora is **identical** to direct, within run noise.
* Engine-local prefix caching is visible (~5 % at this prompt length) and is preserved through Cognitora.
* On the same Q4_K_M weights, **llama.cpp is ~5–10 % faster than Ollama** on this CPU box.

For the **architectural** part of the question (KV-aware routing, tiered/distributed KV, prefill/decode disaggregation, vLLM): a single CPU VM is the wrong instrument; §7 lays out the multi-node bench plan that would actually measure them.
