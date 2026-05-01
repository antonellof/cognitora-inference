# Bare-metal guide

The fastest path is the one-line installer:

```bash
curl -sSfL https://get.cognitora.dev | sh
cgn-ctl pki bootstrap                 # generates dev PKI material
cgn-ctl install baremetal             # systemd units + config
systemctl enable --now cognitora.target
```

After ~5 seconds:

```bash
curl http://127.0.0.1:8080/v1/models
```

returns the live model list. The next sections describe each step in
more detail.

## What gets installed

| Path                                 | Owner   | Purpose                          |
|--------------------------------------|---------|----------------------------------|
| `/usr/local/bin/cgn-*`               | root    | the six binaries                 |
| `/etc/cognitora/cognitora.toml`      | root    | rendered config (idempotent)     |
| `/etc/cognitora/pki/{ca,leaf}.{crt,key}` | root | dev PKI material            |
| `/etc/cognitora/keys.txt`            | root    | API keys file (sha256 hashes)    |
| `/var/lib/cognitora/`                | cognitora | state (kv, model cache)       |
| `/var/log/cognitora/`                | cognitora | logs                          |
| `/run/cognitora/`                    | cognitora | UDS sockets                   |
| `/etc/systemd/system/cgn-*.service`  | root    | systemd units                    |
| `/etc/systemd/system/cognitora.target` | root  | aggregator                       |

A new `cognitora` system user owns the runtime data; the binaries
themselves stay owned by root.

## Topology

For HA, run `cgn-router` on at least two non-GPU hosts (or behind your
load balancer). `cgn-agent` and `cgn-kvcached` always run **together**
on every GPU host — they share a Unix socket for KV transfers.
`cgn-metrics` can run anywhere reachable from the BMC and the Prom
endpoints.

Example small cluster:

| Host        | Role                                                 |
|-------------|------------------------------------------------------|
| `lb1`       | `cgn-router` × 2 (active/active behind HAProxy)      |
| `gpu1..N`   | `cgn-agent` + `cgn-kvcached` + vLLM (one of each)    |
| `obs1`      | `cgn-metrics` + Prometheus + Grafana                 |
| `etcd1..3`  | etcd cluster                                         |

## Key rotation

```bash
cgn-ctl key create alice                # prints the plaintext token
cgn-ctl key create build-bot --read-only
cgn-ctl key revoke <id>
cgn-ctl key lock                        # disables the file until unlock
```

The keys file is hot-reloaded by the router; no restart needed.

## Upgrade

```bash
curl -sSfL https://get.cognitora.dev | sh -s -- --version v0.2.0
systemctl restart cognitora.target
```

The installer always verifies sha256 + cosign before overwriting
binaries; partial upgrades roll back on signature failure.

## Uninstall

```bash
systemctl disable --now cognitora.target
rm -f /etc/systemd/system/cgn-*.service /etc/systemd/system/cognitora.target
systemctl daemon-reload
rm -rf /etc/cognitora /var/lib/cognitora /var/log/cognitora /run/cognitora
userdel cognitora
rm -f /usr/local/bin/cgn-{ctl,router,agent,kvcached,metrics,operator}
```
