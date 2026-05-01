# Internal gRPC surface

The internal contract between Cognitora binaries lives in
[`proto/cognitora/v1/`](../../proto/cognitora/v1). Buf generates Rust
stubs into `cgn-proto`; every other crate consumes the trait imports
from there.

## Services

| Service                      | Server            | Clients                          | Wire           |
|------------------------------|-------------------|----------------------------------|----------------|
| `cognitora.v1.Router`        | `cgn-router`      | other routers, `cgn-operator`    | gRPC mTLS      |
| `cognitora.v1.Agent`         | `cgn-agent`       | `cgn-router`                     | gRPC mTLS      |
| `cognitora.v1.Kv`            | `cgn-kvcached`    | `cgn-agent` (UDS), peer kvcached | gRPC mTLS + QUIC |
| `cognitora.v1.Control`       | `cgn-router`      | `cgn-ctl`, `cgn-operator`        | gRPC mTLS      |

## Messages

The proto package is intentionally small — every wire message has a
single, non-nullable canonical form. The full set is in
`proto/cognitora/v1/`:

- `common.proto` — `NodeRef`, `NodeRole`, `NodeHealth`, `Token`, `Status`
- `router.proto` — `GenerateRequest`, `EmbedRequest`, `RoutingDecision`
- `agent.proto` — `AgentGenerateRequest`, `ModelSpec`, `KvHandoffSpec`
- `kv.proto` — `BlockAddress`, `BlockMeta`, `LookupRequest/Response`
- `control.proto` — admin operations driven by `cgn-ctl`

## TLS

Every cross-process gRPC call requires mTLS. Inside the same host,
`cgn-agent` ↔ `cgn-kvcached` may use a Unix Domain Socket (no TLS,
filesystem permissions guard the channel). The `[security]` block in
`cognitora.toml` decides which path applies.

## Protobuf governance

`buf.yaml` and `buf.lock` live at the repo root. CI runs:

```
buf lint
buf breaking --against '.git#branch=main'
```

so a backwards-incompatible change to any `.proto` is caught before
merge. Field numbers are reserved on removal; new fields use
unallocated numbers.

## Generating clients in other languages

The Rust stubs are generated automatically; for other languages run
buf locally:

```bash
buf generate --template buf.gen.python.yaml
buf generate --template buf.gen.go.yaml
buf generate --template buf.gen.ts.yaml
```

Templates ship for Python, Go, and TypeScript when the corresponding
release tag is cut. The Cognitora maintainers do not own those
client libraries — pin the generated tarball to a Cognitora release
and you get reproducible types.

## Backwards-compatibility policy

Until v1.0:

- Field renames are breaking; field number reuse is breaking; field
  removal goes through the `reserved` keyword first.
- New methods are non-breaking and may land in any minor release.
- Enum value removal is breaking; enum value addition is non-breaking
  and clients must accept unknown values gracefully (the proto
  generated code does this by default).

After v1.0:

- The above plus: removing a method is a major-version bump.
- A `cognitora.v2` package will live alongside `cognitora.v1` for at
  least two minor releases before v1 is removed.
