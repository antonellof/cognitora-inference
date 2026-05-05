# cgn-k8s

[![crates.io](https://img.shields.io/crates/v/cgn-k8s.svg)](https://crates.io/crates/cgn-k8s)
[![docs.rs](https://docs.rs/cgn-k8s/badge.svg)](https://docs.rs/cgn-k8s)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

`kube-rs` helpers and the Custom Resource Definition types reconciled by
[`cgn-operator`](https://crates.io/crates/cgn-operator).

Three CRDs, all under `cognitora.dev/v1alpha1`:

| Kind                | What it represents                                                |
|---------------------|-------------------------------------------------------------------|
| `InferenceCluster`  | Top-level desired state for a Cognitora install in a namespace.   |
| `ModelPool`         | Declarative model loading (cascade, replicas, tensor parallelism).|
| `RoutingPolicy`     | Score weights and admission tunables.                             |

Co-locating the types here means rustc enforces a single source of truth
for the schemas. The yaml manifests under
[`deploy/kubernetes/crds/`](https://github.com/antonellof/cognitora-inference/tree/main/deploy/kubernetes/crds)
are generated from these types via `cgn-ctl pki crd-export`.

## Use

```toml
[dependencies]
cgn-k8s = "0.1"
```

```rust
use cgn_k8s::{InferenceCluster, ModelPool, RoutingPolicy};
use kube::api::Api;

let clusters: Api<InferenceCluster> = Api::namespaced(client, "cognitora");
```

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
