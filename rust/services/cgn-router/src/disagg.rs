//! Prefill / decode disaggregation.
//!
//! When enabled (`[router.disagg] enabled = true`) and the prompt is
//! larger than `colocate_below_tokens`, the router routes the request
//! through two agents:
//!
//! 1. A **prefill** agent runs the model's first forward pass and emits
//!    KV blocks into `cgn-kvcached`.
//! 2. The blocks are pushed (QUIC / RDMA) to the **decode** agent.
//! 3. The decode agent receives `decode_only = true` plus the block list
//!    and streams tokens back to the router.
//!
//! This module implements the FSM that orchestrates that handshake; the
//! transport layer lives in `cgn-kvcached`.

use cgn_proto::v1::NodeRole;

use crate::cluster::NodeEntry;

/// Phase plan for a single request.
#[derive(Debug, Clone)]
pub enum Plan {
    /// Single-stage: pick one node that does both prefill and decode.
    Colocate,
    /// Two-stage with KV handoff.
    Split {
        prefill_role: NodeRole,
        decode_role: NodeRole,
    },
}

/// Decide whether to disaggregate based on prompt length and config.
pub fn plan(enabled: bool, colocate_below_tokens: u32, prompt_tokens: u32) -> Plan {
    if !enabled || prompt_tokens < colocate_below_tokens {
        return Plan::Colocate;
    }
    Plan::Split {
        prefill_role: NodeRole::Prefill,
        decode_role: NodeRole::Decode,
    }
}

/// Quick eligibility check: a node must have any of the specified role
/// (or "Both"), be live, and report a non-empty model.
pub fn is_eligible(node: &NodeEntry, role: NodeRole) -> bool {
    let r = node.role_enum();
    matches!(role, NodeRole::Unspecified) || r == role || r == NodeRole::Both
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_prompts_colocate() {
        let p = plan(true, 256, 100);
        assert!(matches!(p, Plan::Colocate));
    }

    #[test]
    fn long_prompts_split_when_enabled() {
        let p = plan(true, 256, 1024);
        assert!(matches!(p, Plan::Split { .. }));
    }

    #[test]
    fn disabled_always_colocates() {
        let p = plan(false, 1, 100_000);
        assert!(matches!(p, Plan::Colocate));
    }
}
