# cgn-ctl — Admin CLI

**Operations entrypoint** for PKI, API keys, cluster policy, recipe workflows, and installer rendering. Same binary ships in release tarballs beside the daemons.

## Overview

`cgn-ctl` mirrors what automation would do against etcd and the filesystem: bootstrap TLS material, mint scoped API keys, push routing weights, discover and launch [Recipes](../guides/recipes.md), and apply Kubernetes manifests generated from live cluster settings (`install` subcommands — see `--help` and [Bare metal](../guides/baremetal.md)).

It does **not** replace Helm for full GitOps flows — use [`cgn-operator`](cgn-operator.md) when you want CRD-driven reconciliation — but it is the supported tool for laptops, bring-up scripts, and emergency changes.

## Features (selected)

- **`cgn-ctl pki`** — generate dev / lab certificates (`bootstrap`, SAN editing)
- **`cgn-ctl key`** — API key issuance against `api_keys_file` format consumed by `[auth]`
- **`cgn-ctl cluster`** — introspection and policy updates (`set-policy` writes etcd routing weights)
- **`cgn-ctl recipe`** — list/show/up/down wrappers around `recipes/*/up.sh`
- **`cgn-ctl install`** — render systemd / Kubernetes assets from the live configuration (see CLI help for current flags)

Run `cgn-ctl --help` and `cgn-ctl <subcommand> --help` for the authoritative flag list (surface evolves faster than prose docs).

## Architecture

`Operator / human → cgn-ctl → etcd | fs | kubectl | helm` depending on subcommand. No long-running server — pure CLI.

## Configuration

`cgn-ctl` reads global flags and optional `COGNITORA_*` env overrides like other binaries; many subcommands accept **`--config /etc/cognitora/cognitora.toml`** to inherit `[cluster].etcd` endpoints. See [Environment variables](../reference/env.md).

## Example

```bash
# PKI + key material for a lab router
cgn-ctl pki bootstrap --out /tmp/pki --san localhost

# Mint an API key into the file referenced by [auth].api_keys_file
cgn-ctl key create --file /tmp/cognitora/api-keys --scopes "chat,embed"

# Live tuning of routing weights (non-K8s clusters)
cgn-ctl cluster set-policy --kv 0.6 --load 0.2 --power 0.1 --capacity 0.1

# Discover bundled GPU recipes
cgn-ctl recipe ls
```

## Dependencies

- **etcd** — for cluster-wide commands
- **Kubernetes API** — only when invoking install/render paths targeted at K8s

## Related documentation

- [Quickstart](../guides/quickstart.md)
- [Recipes](../guides/recipes.md)
- [Bare metal](../guides/baremetal.md)
- [Kubernetes](../guides/kubernetes.md)

**Source:** [`rust/services/cgn-ctl/`](../../rust/services/cgn-ctl/)
