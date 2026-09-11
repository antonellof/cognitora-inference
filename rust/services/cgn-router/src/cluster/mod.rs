//! Cluster membership + policy distribution.

pub mod cache_state;
mod gossip;
mod registry;
mod watcher;

pub use gossip::run_gossip_watcher;
pub use registry::{NodeEntry, NodeRegistry};
pub use watcher::run_etcd_watcher;
