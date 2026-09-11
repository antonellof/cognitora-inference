# Gossip discovery: etcd-free multi-node clusters

`state_backend = "gossip"` replaces etcd with a UDP gossip mesh for node
discovery and liveness. It removes the last external dependency from a
multi-node cluster: point every node at one or two seed addresses and the
membership converges on its own.

The implementation lives in
[`rust/libraries/cgn-gossip`](../../rust/libraries/cgn-gossip/), a thin
wrapper around [chitchat](https://github.com/quickwit-oss/chitchat), the
scuttlebutt-with-phi-accrual crate built for Quickwit.

## When to use which backend

| | etcd (default) | gossip |
|---|---|---|
| External dependency | etcd (multi-node only) | none |
| Liveness | 15s lease, expired by etcd | phi-accrual failure detector |
| Node records | `/cognitora/nodes/<id>` keys, watched | `cgn.node` key gossiped per member |
| Confirmed-KV prefix feed | yes (`/cognitora/kv/...` claims) | no (optimistic prefix inserts only) |
| Cordon / drain flags | yes | no |
| Routing policy hot-reload | yes (`routing/policy` watch) | no (TOML score weights at startup) |
| Autoscaler / operator hints | yes | no |
| Pipeline-worker entries | yes | no (single entry per node) |

Gossip mode trades the etcd-backed control-plane features for zero
infrastructure. It fits edge deployments, lab clusters, and small fleets
where installing etcd is more work than the features are worth. Larger
clusters that want cordoning, confirmed-KV routing, or the autoscaler
should stay on the default backend.

## How it works

Both backends move the same document: the JSON node record built by
`cgn-agent`'s `node_record_json` helper (node id, addresses, readiness,
GPU identity, queue depth, KV block counts, engine stats). One builder
feeds both paths, so the two backends cannot drift.

- **Agent** (`cgn-agent/src/health.rs`): in gossip mode `loop_emit`
  spawns a `GossipMember` bound to `cluster.gossip_listen` and republishes
  the node record on the same 5s heartbeat cadence used for etcd. There
  is no lease; peers detect death through the phi-accrual failure
  detector instead.
- **Router** (`cgn-router/src/cluster/gossip.rs`): the router joins the
  mesh as a record-less member. It publishes no `cgn.node` key, so it is
  never a routing candidate. Every 2.5s (half the heartbeat cadence) it
  reconciles the live members' records into the in-memory `NodeRegistry`,
  the same registry the etcd watcher feeds.

The gossip protocol runs over UDP on port 7946 by default (`ports::GOSSIP_UDP`),
with a 1s gossip interval. Failure detection uses chitchat's default
phi-accrual settings (phi threshold 8.0).

## Configuration

```toml
[cluster]
state_backend = "gossip"

# Any subset of live members works as seeds; leave empty on the first node.
gossip_seeds = ["10.0.0.10:7946", "10.0.0.11:7946"]

# UDP socket the gossip member binds.
gossip_listen = "0.0.0.0:7946"

# Address peers use to reach this member. Required for multi-host
# clusters when gossip_listen binds a wildcard address.
gossip_advertise = "10.0.0.12:7946"
```

See the [configuration reference](../reference/config.md) for key-by-key
details.

## Failure model

- A node that stops gossiping is marked dead by the phi-accrual detector
  and drops out of the router's `NodeRegistry` on the next 2.5s sync.
  There is no fixed TTL: the detector adapts to observed heartbeat
  jitter, so flaky links do not cause flapping the way a hard lease
  timeout would.
- Network partitions heal automatically: chitchat's scuttlebutt
  reconciliation exchanges only the deltas each side is missing once
  connectivity returns.
- The router restarts its gossip watcher after 5s if the member fails to
  spawn (for example, when the UDP port is taken).

## Explicit non-goals

Gossip mode deliberately does not replicate etcd's consistency
guarantees. It is an eventually-consistent membership plane, not a
coordination plane. Anything that needs linearizable writes (cordon
flags, KV claims, policy hot-reload, operator state) remains etcd-only
by design.
