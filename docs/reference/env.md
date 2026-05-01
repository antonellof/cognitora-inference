# Environment variables

Every TOML key in [`configs/cognitora.toml.example`](../../configs/cognitora.toml.example)
has a matching environment variable. Cognitora layers values in this
order, last-wins:

1. Compiled defaults (in `cgn-core::config`).
2. TOML file at `--config` / `$COGNITORA_CONFIG` / `/etc/cognitora/cognitora.toml`.
3. Environment variables matching `COGNITORA__<SECTION>__<KEY>`.
4. Command-line flags.

## Naming rule

```
COGNITORA__<section>__<key>          # one section
COGNITORA__<sec>__<sub>__<key>       # nested
```

Always two underscores between segments, SCREAMING_SNAKE for the
keys. Booleans accept `true`/`false`/`1`/`0`. Durations accept the
humantime forms (`30s`, `5m`, `1h`).

## Examples

| Variable                                        | Maps to                              |
|-------------------------------------------------|--------------------------------------|
| `COGNITORA__CLUSTER__NAME`                      | `[cluster].name`                     |
| `COGNITORA__SECURITY__REQUIRE_MTLS`             | `[security].require_mtls`            |
| `COGNITORA__AUTH__ENABLED`                      | `[auth].enabled`                     |
| `COGNITORA__AUTH__OIDC_ISSUER`                  | `[auth].oidc_issuer`                 |
| `COGNITORA__ROUTER__LISTEN_HTTP`                | `[router].listen_http`               |
| `COGNITORA__ROUTER__SCORE_WEIGHTS__KV`          | `[router.score_weights].kv`          |
| `COGNITORA__ROUTER__ADMISSION__MAX_QUEUE`       | `[router.admission].max_queue`       |
| `COGNITORA__ROUTER__CASCADE__ENABLED`           | `[router.cascade].enabled`           |
| `COGNITORA__AGENT__ENGINE_BINARY`               | `[agent].engine_binary`              |
| `COGNITORA__KV__RAM_GIB`                        | `[kv].ram_gib`                       |
| `COGNITORA__METRICS__SCRAPE_INTERVAL`           | `[metrics].scrape_interval`          |

## Standard runtime variables

These aren't Cognitora-specific but shape the runtime:

| Variable                          | Effect                                      |
|-----------------------------------|---------------------------------------------|
| `RUST_LOG`                        | overrides `tracing` filter (e.g. `cgn_router=debug`) |
| `OTEL_EXPORTER_OTLP_ENDPOINT`     | enable OTLP/gRPC traces                     |
| `OTEL_TRACES_SAMPLER_ARG`         | sample rate (0.0 – 1.0)                     |
| `OTEL_RESOURCE_ATTRIBUTES`        | extra resource tags                         |
| `TOKIO_WORKER_THREADS`            | overrides `[router].worker_threads = 0`     |
| `RUST_BACKTRACE`                  | `1` / `full` for crash diagnostics          |

## Secrets-friendly knobs

Some configs accept a `*_FILE` variant that reads the value from disk
so you can mount Kubernetes Secrets without quoting them:

| Variable                                       | Reads                                   |
|------------------------------------------------|-----------------------------------------|
| `COGNITORA__AUTH__API_KEYS_FILE`               | path to sha256 keys file                |
| `COGNITORA__SECURITY__KEY_FILE`                | path to PEM-encoded TLS private key     |
| `COGNITORA__METRICS__REDFISH_PASS_FILE`        | path to a file containing the password  |
