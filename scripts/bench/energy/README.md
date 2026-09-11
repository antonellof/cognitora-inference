# Energy benchmark harness

Measures **tokens/s per watt** and **J/token** while driving streaming
chat completions through `cgn-router`. Power comes from
`cgn_cluster_node_power_watts` (summed by the router into
`cgn_cluster_power_watts_total`); token counts from
`cgn_router_chat_completion_tokens_total`.

## Local validation (no GPU)

```bash
bash scripts/bench/validate-local.sh
```

Exercises the summarize math with fixture metrics. Publishable J/token
numbers require a GPU host with power reporting (below).

## Prerequisites (GPU host)

* A running Cognitora stack with agents reporting power in heartbeats
  (NVML / Redfish / rocm-smi).
* `python3`, `curl` (optional sanity checks).

## Running

Assumes the router admin listener is on `:9091` and the OpenAI surface
on `:8080`:

```bash
bash scripts/bench/energy/run.sh
```

Knobs (env): `N`, `CONC`, `MAX_TOKENS`, `MODEL`, `ROUTER_URL`, `ADMIN_URL`,
`OUT_DIR`.

## Outputs

Written to `scripts/bench/energy/results/` (override with `OUT_DIR`):

| File | Contents |
|------|----------|
| `before.json` / `after.json` | Prometheus samples around the run |
| `bench.json` | Raw `bench_client.py` record |
| `results.json` | Combined metadata + computed energy metrics |
| `summary.md` | Markdown table for publishing |

## Published results

**Pending first GPU run** — populate the table in `summary.md` only from
an actual run on reference hardware.
