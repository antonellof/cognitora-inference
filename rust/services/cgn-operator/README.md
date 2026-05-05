# cgn-operator

[![crates.io](https://img.shields.io/crates/v/cgn-operator.svg)](https://crates.io/crates/cgn-operator)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Kubernetes operator for [Cognitora](https://github.com/antonellof/cognitora-inference),
built on `kube-rs`.

Reconciles three CRDs (defined in
[`cgn-k8s`](https://crates.io/crates/cgn-k8s), under
`cognitora.dev/v1alpha1`):

* `InferenceCluster` — top-level desired state. Owns the router
  StatefulSet/Deployment, agent DaemonSet, kvcached Deployment, and the
  metrics Deployment.
* `ModelPool` — declarative model loading. Translates to
  `cgn-ctl model load` invocations against the cluster.
* `RoutingPolicy` — score weights and admission tunables, written into
  etcd at `/cognitora/routing/policy`.

The operator also runs the autoscaler driven by SLA + energy signals
exported by [`cgn-metrics`](https://crates.io/crates/cgn-metrics).

## Install

The recommended install is via Helm (CRDs and operator together):

```bash
helm install cognitora oci://ghcr.io/antonellof/charts/cognitora -n cognitora --create-namespace
```

To get just this binary:

```bash
cargo install cgn-operator
```

## Run

```bash
cgn-operator --config /etc/cognitora/cognitora.toml
```

See [`docs/operations/kubernetes.md`](https://github.com/antonellof/cognitora-inference/blob/main/docs/operations/kubernetes.md)
and the CRD manifests under
[`deploy/kubernetes/crds/`](https://github.com/antonellof/cognitora-inference/tree/main/deploy/kubernetes/crds).

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).
