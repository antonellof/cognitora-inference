# Configuration reference

Cognitora binaries read a single TOML file (default
`/etc/cognitora/cognitora.toml`) plus environment overrides. The
authoritative schema lives in
[`rust/libraries/cgn-core/src/config.rs`](../../rust/libraries/cgn-core/src/config.rs);
the canonical example with every section documented inline lives at
[`configs/cognitora.toml.example`](../../configs/cognitora.toml.example).

Per-binary narrative docs (features, ports, dependencies): see
[`docs/platform/`](../platform/README.md).

## Sections

| Section            | Owner crate         | Required by                                  |
|--------------------|---------------------|----------------------------------------------|
| `[cluster]`        | `cgn-core::config`  | every binary                                 |
| `[security]`       | `cgn-tls`           | every binary that opens mTLS                 |
| `[auth]`           | `cgn-auth`          | `cgn-router`                                 |
| `[router.*]`       | `cgn-router`        | `cgn-router`                                 |
| `[agent.*]`        | `cgn-agent`         | `cgn-agent`                                  |
| `[engine.*]`       | `cgn-agent`         | `cgn-agent` (which engine to spawn / proxy)  |
| `[kv.*]`           | `cgn-kv`            | `cgn-kvcached`                               |
| `[metrics.*]`      | `cgn-metrics`       | `cgn-metrics`                                |
| `[models.<name>]`  | `cgn-core::config`  | `cgn-router` (declarative model registry)    |

## `[engine]` — pluggable inference engine

Cognitora's `cgn-agent` is engine-agnostic: any process that exposes the
OpenAI HTTP surface (`/v1/completions`, `/health`, `/v1/models`) plugs in.

| Key                     | Type   | Default                          | Notes |
|-------------------------|--------|----------------------------------|-------|
| `engine.kind`           | enum   | `"vllm"`                         | One of `vllm`, `sglang`, `llama_cpp`, `mlx`, `cgn_infer`, `openai_compat`. |
| `engine.url`            | string | `http://127.0.0.1:8000`          | OpenAI HTTP base URL. |
| `engine.kv_offload`     | enum   | `"none"`                         | Engine-side KV offload backend. One of `none`, `nixl`, `lmcache`, `hicache`, `kvbm`. See [Engine-side KV offload](#engine-side-kv-offload) below. |
| `engine.vllm.binary`    | string | `"vllm"`                         | Path or PATH-name of the `vllm` CLI. |
| `engine.vllm.extra_args`| array  | `["--enable-chunked-prefill"]`   | Appended after the auto-rendered argv. |
| `engine.sglang.binary`             | string | `"python"`                | Python interpreter that runs `-m sglang.launch_server`. |
| `engine.sglang.host`               | string | `"127.0.0.1"`             | Where the engine listens. |
| `engine.sglang.port`               | u16    | `8000`                    | Must match `engine.url`. |
| `engine.sglang.context_length`     | u32    | `4096`                    | Default context window when `[models.\*].max_model_len` is unset. |
| `engine.sglang.mem_fraction_static`| f32    | `0.85`                    | Mem fraction for SGLang's RadixAttention KV pool. |
| `engine.sglang.extra_args`         | array  | `[]`                      | Appended after the auto-rendered argv. Pass `--enable-radix-cache` here. |
| `engine.llama_cpp.binary`     | string | `"python"`                  | Python interpreter (`mode = python_server`) or `llama-server` binary (`mode = binary`). |
| `engine.llama_cpp.mode`       | enum   | `"python_server"`           | `python_server` or `binary`. |
| `engine.llama_cpp.host`       | string | `"127.0.0.1"`               | Where the engine listens. |
| `engine.llama_cpp.port`       | u16    | `8000`                      | Must match `engine.url`. |
| `engine.llama_cpp.n_ctx`      | u32    | `4096`                      | Context window. |
| `engine.llama_cpp.n_threads`  | u32    | `4`                         | CPU thread count. |
| `engine.llama_cpp.n_gpu_layers` | i32  | `0`                         | `0` = CPU only, `-1` = all to GPU. |
| `engine.llama_cpp.extra_args` | array  | `[]`                        | Extra flags passed to the engine. |
| `engine.mlx_lm.binary`    | string | `"python3"`                 | Python that can `import mlx_lm`. |
| `engine.mlx_lm.host`      | string | `"127.0.0.1"`               | `--host` for `mlx_lm.server`. |
| `engine.mlx_lm.port`      | u16    | `8090`                      | `--port`; default avoids clashing with `ROUTER_HTTP` (8080). Must match `engine.url`. |
| `engine.mlx_lm.extra_args` | array | `[]`                       | Appended after `--model …`. |
| `engine.cgn_infer.binary`   | string | `"cgn-infer"`              | Path or PATH-name of the `cgn-infer` binary. |
| `engine.cgn_infer.host`     | string | `"127.0.0.1"`              | `--host` for `cgn-infer serve`. |
| `engine.cgn_infer.port`     | u16    | `8001`                     | `--port`. Must match `engine.url`. |
| `engine.cgn_infer.ctx`      | u32    | `4096`                     | `--ctx` context window. |
| `engine.cgn_infer.threads`  | u32    | `4`                        | `--threads` CPU thread count. |
| `engine.cgn_infer.extra_args` | array | `[]`                      | Appended after the auto-rendered argv. |

When `kind = "openai_compat"` the agent does **not** spawn a child process;
it only proxies to whatever is at `engine.url`. Use this with systemd /
Kubernetes / a sidecar that owns the engine lifecycle.

### Engine selection

The supported engine kinds map to the same OpenAI HTTP surface, so they
are fully interchangeable from the router's perspective:

* **`vllm`** — `vllm serve <model> --tensor-parallel-size <N> ...`. Best
  general-purpose GPU engine; supports continuous batching and chunked
  prefill out of the box.
* **`sglang`** — `python -m sglang.launch_server --model-path <model>
  --tp <N> ...`. Adds RadixAttention prefix caching that complements
  Cognitora's *cross-node* prefix routing — the router still picks the
  node with the longest cached prefix, and SGLang then reuses cache
  inside that node.
* **`llama_cpp`** — CPU-friendly fallback (and CUDA-offload via
  `n_gpu_layers`); useful for laptops, CI, and edge deployments.
* **`mlx`** — `python3 -m mlx_lm.server --model <hf_or_path> --host <h> --port <p> …`.
  **Apple Silicon / macOS only** ([mlx-lm](https://github.com/ml-explore/mlx-lm)).
  Use `kv_offload = "none"` only.
* **`cgn_infer`** *(experimental / preview)* — `cgn-infer serve --model
  <gguf> --host <h> --port <p> --ctx <n> --threads <n>`. Cognitora's
  **first-party native engine** (Rust + Candle, GGUF via mmap) with
  continuous batching (llama/qwen2 GGUFs; qwen3/gemma3/phi3/MoE serve
  sequentially) and optional multi-node layer-pipeline mode via
  `[models.*.pipeline]`; only `kv_offload = "none"` is valid. See
  [`docs/architecture/cgn-infer.md`](../architecture/cgn-infer.md).
* **`openai_compat`** — proxy-only.

### Engine-side KV offload

`engine.kv_offload` selects which connector `cgn-agent` injects when
spawning the engine. The router is unaware of this dial — it only sees
prefix-overlap signals via `cgn-kvcached` either way — so swapping
backends is a one-line change.

| Value     | Effect (vLLM)                                                                                       | Effect (SGLang)                                                                          |
|-----------|------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------|
| `none`    | nothing injected                                                                                     | nothing injected                                                                          |
| `nixl`    | `--kv-transfer-config '{"kv_connector":"NixlConnector",...}'` with role-aware `kv_role`              | (rejected — SGLang HiCache uses NIXL internally; pick `hicache` instead)                  |
| `lmcache` | `LMCacheConnectorV1` (agg) or `PdConnector(LMCache+NIXL)` (disagg, prefill role)                     | (rejected — LMCache is vLLM-side)                                                         |
| `hicache` | (rejected — vLLM has no HiCache)                                                                     | `--enable-hierarchical-cache --hicache-ratio 2 --hicache-write-policy write_through --hicache-storage-backend nixl` |
| `kvbm`    | `--kv-transfer-config '{"kv_connector":"DynamoConnector","kv_connector_module_path":"kvbm.vllm_integration.connector",...}'` | (rejected — KVBM has no SGLang support)                                                   |

Disagg topologies (`[agent].role = "prefill"` or `"decode"`) compose
the chosen backend with NIXL automatically. The full table — including
the exact JSON blobs — lives in [`docs/architecture/kv-strategy.md`](../architecture/kv-strategy.md).

LMCache, HiCache, and KVBM all require the corresponding Python
package to be installed in the engine's virtualenv. `cgn-agent` does
not install them; the recipe's `up.sh` warns when they're missing.

### Per-model knobs

`[models."<name>"].path` is required when `engine.kind = "llama_cpp"` or
`"cgn_infer"` (the filesystem path to a `.gguf` file). For SGLang or **MLX**, `path` is optional: when
unset the spawn argv uses the model table key as the Hugging Face repo id; when set,
it is passed to `--model` as a local directory. vLLM behaves the same way as SGLang for `path`.

#### Capability constraints (heterogeneous fleets)

Per-model hardware constraints for clusters mixing GPU generations or
vendors. The router filters routing candidates against the GPU identity
each agent publishes in its heartbeat (NVML on NVIDIA, `rocm-smi` on
AMD). Nodes that report **no** GPU identity (older agents, CPU boxes)
are never filtered — the constraint only excludes nodes that
affirmatively report incompatible hardware.

```toml
[models."llama3-70b"]
min_vram_mb = 80000     # only nodes reporting ≥ 80 GB of GPU memory
require_gpu = "h100"    # case-insensitive substring of GPU name or vendor
```

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `min_vram_mb` | u64 | unset | Minimum total GPU memory (MiB) a node must report. |
| `require_gpu` | string | unset | Substring the node's GPU name or vendor must contain (`"h100"`, `"mi300"`, `"nvidia"`, `"amd"`). |

#### `[agent].watt_limit` — soft power cap

```toml
[agent]
watt_limit = 700.0   # watts; 0 (default) = uncapped
```

Published in the heartbeat and mirrored to
`cgn_cluster_node_watt_limit`. When at least one candidate node is under
its cap, the router routes only to under-cap nodes; when *every*
candidate is over, routing proceeds anyway — serving beats browning out
a request. Combine with the autoscaler's `high_watt_threshold` for
drain-based enforcement.

#### `[router.federation]` — cross-cluster fallback

```toml
[router.federation]
enabled = true
peers = ["https://us-west.cognitora.example:7070",
         "https://eu-central.cognitora.example:7070"]
```

When the local cluster has no eligible node for a request's model, the
gateway forwards the request to a peer cluster's router over gRPC
(mTLS). Peers are probed concurrently and the lowest-connect-latency
reachable peer wins. Forwarding targets the peer's gRPC surface, which
only routes locally — a request crosses at most one cluster boundary
and cannot loop. Forwards are counted in
`cgn_router_federation_forwards_total{model,peer}`.

### `[models.*.pipeline]` — cgn-infer layer pipeline

With `engine.kind = "cgn_infer"`, a per-model `pipeline` block splits
the model across processes/nodes by layer range. The agent spawns one
`cgn-infer worker` per `spawn = true` member (before the coordinator),
registers workers in etcd as non-servable, and restarts the whole
pipeline if any member exits.

```toml
[models."llama3-8b"]
path = "/models/llama3-8b.gguf"

[models."llama3-8b".pipeline]
coordinator_layers  = "0:11"    # embedding + layers [0,11) + LM head, local
activation_encoding = "f16"     # or "int8"

[[models."llama3-8b".pipeline.workers]]
listen = "127.0.0.1:9101"       # spawned locally by this agent
layers = "11:22"

[[models."llama3-8b".pipeline.workers]]
listen   = "10.0.0.5:9101"      # managed elsewhere (another agent / systemd)
layers   = "22:32"
endpoint = "http://10.0.0.5:9101"
spawn    = false
```

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `coordinator_layers` | string | — | Coordinator's local slice `"A:B"` (half-open, must start at 0). |
| `activation_encoding` | string | `"f16"` | Hidden-state wire encoding: `f16` or `int8`. |
| `workers[].listen` | string | — | Worker gRPC bind address (`host:port`). |
| `workers[].layers` | string | — | Worker's layer slice `"A:B"`. |
| `workers[].endpoint` | string | `http://<listen>` | URL the coordinator dials. |
| `workers[].spawn` | bool | `true` | Spawn locally, or expect an externally managed worker. |

The coordinator validates at startup that `coordinator_layers` plus
the worker slices, in order, exactly tile the model's layer count.
Pipeline mode is limited to `llama`/`qwen2` GGUF architectures.

### Legacy aliases

`[agent].vllm_url` and `[agent].vllm_cmd` from older configs still work
but emit a one-time warning. Migrate them to `[engine].url` and
`[engine.vllm].extra_args` respectively.

## Overrides

Every TOML key has a corresponding environment variable: prepend
`COGNITORA__`, separate sections with `__`, and use SCREAMING_SNAKE.

```bash
# Override [router].listen_http
COGNITORA__ROUTER__LISTEN_HTTP=0.0.0.0:8000

# Disable auth for a dev run
COGNITORA__AUTH__ENABLED=false
```

CLI flags take precedence over the env, which takes precedence over the
TOML file, which takes precedence over compiled defaults.

## Hot reload

The following keys reload *without* restart:

* `[auth].api_keys_file` (sha256 keys file is watched and re-read)
* `[router.score_weights]` (router subscribes to etcd
  `/cognitora/routing/policy`)
* `[router.cascade]` and `[router.disagg]` (same etcd key)

Everything else requires `systemctl restart cgn-<binary>` or, in K8s, a
rolling restart of the corresponding deployment / DaemonSet.
