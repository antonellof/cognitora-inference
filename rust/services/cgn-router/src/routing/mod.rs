//! KV-aware routing.
//!
//! Public API:
//!
//! ```ignore
//! let decision = routing::pick(state, &request).await?;
//! ```
//!
//! `pick` returns the chosen node + the per-node debug score map for
//! observability. The routing decision is fully deterministic given the
//! prefix index and the node registry — see [`score`].

pub mod grpc;
pub mod score;
pub mod selector;

pub use score::{score_node, Score};
pub use selector::{pick, pick_pair, RoutingDecision};
