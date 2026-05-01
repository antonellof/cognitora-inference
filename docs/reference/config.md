# Configuration reference

Cognitora binaries read a single TOML file (default
`/etc/cognitora/cognitora.toml`) plus environment overrides. The
authoritative schema lives in
[`rust/libraries/cgn-core/src/config.rs`](../../rust/libraries/cgn-core/src/config.rs);
the canonical example with every section documented inline lives at
[`configs/cognitora.toml.example`](../../configs/cognitora.toml.example).

## Sections

| Section            | Owner crate         | Required by                                  |
|--------------------|---------------------|----------------------------------------------|
| `[cluster]`        | `cgn-core::config`  | every binary                                 |
| `[security]`       | `cgn-tls`           | every binary that opens mTLS                 |
| `[auth]`           | `cgn-auth`          | `cgn-router`                                 |
| `[router.*]`       | `cgn-router`        | `cgn-router`                                 |
| `[agent.*]`        | `cgn-agent`         | `cgn-agent`                                  |
| `[kv.*]`           | `cgn-kv`            | `cgn-kvcached`                               |
| `[metrics.*]`      | `cgn-metrics`       | `cgn-metrics`                                |
| `[models.<name>]`  | `cgn-core::config`  | `cgn-router` (declarative model registry)    |

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
