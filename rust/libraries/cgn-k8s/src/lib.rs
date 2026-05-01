//! Cognitora Kubernetes types and helpers.
//!
//! Defines the three CRDs reconciled by `cgn-operator`:
//!
//! * [`InferenceCluster`] — top-level desired state for a Cognitora install
//!   in a namespace (router replicas, agents, kvcached, metrics).
//! * [`ModelPool`]        — declarative model loading (cascade, replicas, tp).
//! * [`RoutingPolicy`]    — score weights and admission tunables.
//!
//! Co-located with the operator so rustc enforces a single source of truth
//! for CRD schemas. Yaml manifests under `deploy/kubernetes/crds/` are
//! generated from these types via `cgn-ctl pki crd-export`.

#![forbid(unsafe_code)]

pub mod crds;
pub mod helpers;

pub use crds::{InferenceCluster, ModelPool, RoutingPolicy};
