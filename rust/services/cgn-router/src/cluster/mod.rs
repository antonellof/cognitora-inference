//! Cluster membership + policy distribution.

mod registry;
mod watcher;

pub use registry::{NodeEntry, NodeRegistry};
pub use watcher::run_etcd_watcher;
