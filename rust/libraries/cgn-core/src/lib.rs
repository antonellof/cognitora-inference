//! Cognitora shared library.
//!
//! Lean cross-cutting concerns reused by every binary: configuration,
//! errors, BLAKE3 prefix hashing, and the concurrent prefix index.
//!
//! Telemetry, TLS, and Kubernetes helpers live in dedicated `cgn-*`
//! crates so this crate stays free of heavy transitive dependencies.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod hash;
pub mod prefix;

pub use error::{exit_code, Error, Result};

/// Default UNIX domain socket directory used by the daemons.
pub const DEFAULT_UDS_DIR: &str = "/run/cognitora";

/// Default config search path.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/cognitora/cognitora.toml";

/// Cluster state key prefixes (etcd) used by router/agent/operator.
pub mod etcd_keys {
    pub const NODES: &str = "/cognitora/nodes/";
    pub const MODELS: &str = "/cognitora/models/";
    pub const ROUTING: &str = "/cognitora/routing/policy";
    pub const ROUTER_LEADER: &str = "/cognitora/router/leader";
    /// User-set per-node cordon flag. The router watcher mirrors the
    /// presence of `<CORDON>{node_id}` into `NodeEntry::cordoned`, and
    /// scoring excludes cordoned nodes from candidate selection.
    pub const CORDON: &str = "/cognitora/cordon/";
}

/// Default ports.
pub mod ports {
    /// `cgn-router` OpenAI-compatible HTTP/SSE.
    pub const ROUTER_HTTP: u16 = 8080;
    /// `cgn-router` admin gRPC (metrics, control RPCs).
    pub const ROUTER_GRPC: u16 = 9090;
    /// `cgn-router` admin HTTP (Prometheus scrape).
    pub const ROUTER_ADMIN: u16 = 9091;
    /// `cgn-agent` gRPC (router → agent).
    pub const AGENT_GRPC: u16 = 7070;
    /// `cgn-kvcached` gRPC (cross-host KV ops).
    pub const KV_GRPC: u16 = 7071;
    /// `cgn-kvcached` QUIC transfer port.
    pub const KV_QUIC: u16 = 7072;
    /// `cgn-metrics` HTTP scrape.
    pub const METRICS_HTTP: u16 = 9092;
    /// vLLM HTTP (private to the agent).
    pub const VLLM_HTTP: u16 = 8000;
}

/// Build / version metadata.
pub mod build {
    /// Crate version (from Cargo).
    pub const VERSION: &str = env!("CARGO_PKG_VERSION");
}
