# Environment variables

Configuration lives in the TOML file. There are no per-key environment
overrides: Cognitora does not read `COGNITORA__*` variables. Secrets are
referenced by file path in TOML, such as `[auth].api_keys_file` and
`[security].key_file`, so Kubernetes Secrets can be mounted as files.

## Cognitora variables

| Variable | Effect |
|----------|--------|
| `CGN_CONFIG` | Config path when `--config` isn't passed; the fallback is `/etc/cognitora/cognitora.toml` |
| `CGN_CACHE_DIR` | Where downloaded tokenizers are cached; defaults to the system temp directory |
| `CGN_RECIPES_DIR` | Recipe directory for `cgn-ctl recipe` |
| `CGN_HELM_BIN` | `helm` binary for `cgn-ctl` |
| `COGNITORA_ETCD` | etcd endpoint for `cgn-ctl` |
| `HF_TOKEN` | Bearer token for `tokenizer_repo` downloads, needed for gated repos |
| `HF_ENDPOINT` | Hugging Face mirror for `tokenizer_repo` |

## Standard runtime variables

These aren't Cognitora-specific but shape the runtime:

| Variable                          | Effect                                      |
|-----------------------------------|---------------------------------------------|
| `RUST_LOG`                        | overrides `tracing` filter (e.g. `cgn_router=debug`) |
| `OTEL_EXPORTER_OTLP_ENDPOINT`     | enable OTLP/gRPC traces                     |
| `OTEL_TRACES_SAMPLER_ARG`         | sample rate (0.0 – 1.0)                     |
| `OTEL_RESOURCE_ATTRIBUTES`        | extra resource tags                         |
| `TOKIO_WORKER_THREADS`            | Tokio worker count                          |
| `RUST_BACKTRACE`                  | `1` / `full` for crash diagnostics          |
