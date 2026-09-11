//! Cluster membership + policy distribution.

mod gossip;
mod registry;
mod watcher;

pub use gossip::run_gossip_watcher;
pub use registry::{NodeEntry, NodeRegistry};
pub use watcher::run_etcd_watcher;
