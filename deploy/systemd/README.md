# systemd units

Drop-in unit files for bare-metal installs. `cgn-ctl install baremetal`
copies these to `/etc/systemd/system/` and runs `systemctl daemon-reload`.

## Layout

| Unit                     | Runs           | Notes                                      |
|--------------------------|----------------|--------------------------------------------|
| `cgn-router.service`     | every host     | OpenAI gateway + KV-aware orchestrator     |
| `cgn-agent.service`      | every GPU host | Engine supervisor; needs `/dev/nvidia*`    |
| `cgn-kvcached.service`   | every GPU host | RAM/SSD tiers + QUIC; agent depends on it  |
| `cgn-metrics.service`    | one per region | Prometheus federation + power telemetry    |
| `cognitora.target`       | aggregator     | `systemctl start cognitora.target`         |

## Conventions

* All services run as `cognitora:cognitora` (created by the installer).
* All services read `/etc/cognitora/cognitora.toml` plus a per-binary
  `*.env` file for sensitive overrides (API keys, etcd password, etc.).
* Writable paths: `/var/lib/cognitora` (state), `/var/log/cognitora`
  (logs/PIDs), `/run/cognitora` (UDS sockets).
* `cgn-agent` and `cgn-kvcached` need `LimitMEMLOCK=infinity` so the
  RAM tier can pin pages.
* `cgn-router` is intentionally `Type=notify`; it sends `READY=1` once
  HTTP, gRPC, and admin listeners are bound.
