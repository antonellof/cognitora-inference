# Exit codes

All Cognitora binaries follow the same exit-code convention so
systemd, Kubernetes, and shell pipelines can react sensibly.

| Code | Meaning                  | Examples                                                      |
|-----:|--------------------------|---------------------------------------------------------------|
| `0`  | clean exit               | normal `SIGTERM` shutdown after draining in-flight work       |
| `1`  | generic failure          | runtime panic, unhandled error                                |
| `2`  | bad invocation           | unknown flag, malformed `--config` path                       |
| `3`  | configuration invalid    | `cognitora.toml` parse error or schema validation failure     |
| `4`  | dependency unavailable   | etcd unreachable on startup, missing `helm` binary on PATH    |
| `5`  | TLS / PKI error          | cert/key mismatch, expired CA, missing `client_ca_file`       |
| `6`  | engine failure           | `cgn-agent`: engine binary not found, ready probe never green |
| `7`  | port bind failure        | another process is using `[router].listen_http` etc.          |
| `8`  | storage error            | `cgn-kvcached`: index dir not writable, SSD tier path missing |
| `9`  | feature unsupported      | `--features rdma` build needed but called on an unsupported OS |
|`10`  | gracefully drained       | `cgn-agent` exited because `cgn-ctl cluster drain` finished   |
|`130` | `SIGINT`                 | Ctrl-C during interactive run                                 |
|`143` | `SIGTERM`                | normal stop signal under systemd / Kubernetes                 |

## How to use this in systemd

The shipped units already declare:

```ini
Restart=on-failure
SuccessExitStatus=10 130 143
```

so a clean drain (`10`), a Ctrl-C (`130`), or a `systemctl stop`
(`143`) won't trigger a restart, but a real crash (`1`) or a config
error (`3`) will.

## How to use this in Kubernetes

The Helm chart's Deployment spec uses
`terminationMessagePolicy: FallbackToLogsOnError` so a non-zero exit
plus the last log line lands in `kubectl describe pod`. Combined
with the table above this gives a one-glance diagnosis.

## Tests

`rust/libraries/cgn-core/tests/exit_codes.rs` exercises the matrix
above against `cgn_core::exit_code(&Error)`. Every binary's `main()`
funnels its returned `Error` through that function before exiting, so
the systemd `SuccessExitStatus` lists and the Kubernetes runbook stay
in lockstep with the table above.
