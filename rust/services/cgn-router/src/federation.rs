//! Cross-cluster federation.
//!
//! When a request lands on this router but the local cluster has no
//! eligible node for the requested model, the federation layer forwards
//! the request over mTLS gRPC to another Cognitora cluster's router.
//!
//! Configuration:
//!
//! ```toml
//! [router.federation]
//! enabled = true
//! peers = ["https://us-west.cognitora.example:7070",
//!          "https://eu-central.cognitora.example:7070"]
//! ```
//!
//! Today the forwarder runs the same routing decision logic on the
//! peer's snapshot, picks the best peer cluster (lowest queue depth /
//! best cache overlap if known), and proxies the OpenAI request unchanged.

use std::time::{Duration, Instant};

use cgn_core::{Error, Result};
use cgn_proto::v1::{router_client::RouterClient, GenerateRequest};
use tracing::{debug, warn};

/// Per-peer connect budget. A peer that can't complete a gRPC connect in
/// this window is a poor place to send a latency-sensitive request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Dispatch a request to a federated peer. Returns the chosen peer's
/// gRPC endpoint and an open client. Caller is responsible for then
/// streaming the request through that client.
///
/// All peers are probed **concurrently** and the reachable peer with the
/// lowest connect latency wins — connect RTT is a serviceable proxy for
/// geographic proximity without a dedicated stats RPC. Unreachable peers
/// are logged and skipped.
pub async fn pick_peer(
    peers: &[String],
    model: &str,
) -> Result<(String, RouterClient<tonic::transport::Channel>)> {
    if peers.is_empty() {
        return Err(Error::Unavailable("no federation peers configured".into()));
    }
    let probes = peers.iter().map(|peer| {
        let peer = peer.clone();
        async move {
            let started = Instant::now();
            match tokio::time::timeout(CONNECT_TIMEOUT, RouterClient::connect(peer.clone())).await {
                Ok(Ok(client)) => {
                    let rtt = started.elapsed();
                    debug!(%peer, rtt_ms = rtt.as_millis() as u64, "federation peer reachable");
                    Some((rtt, peer, client))
                }
                Ok(Err(e)) => {
                    warn!(%peer, error=?e, "federation peer unreachable");
                    None
                }
                Err(_) => {
                    warn!(%peer, timeout_ms = CONNECT_TIMEOUT.as_millis() as u64,
                          "federation peer connect timed out");
                    None
                }
            }
        }
    });
    let best = futures::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .min_by_key(|(rtt, _, _)| *rtt);
    match best {
        Some((rtt, peer, client)) => {
            debug!(%peer, %model, rtt_ms = rtt.as_millis() as u64, "federation peer selected");
            Ok((peer, client))
        }
        None => Err(Error::Unavailable(format!(
            "all {} federation peers unreachable",
            peers.len()
        ))),
    }
}

/// Forward `req` to `peer` and return its streaming response. Used by
/// the gateway's chat path when the local routing decision yields no
/// eligible node and federation is enabled.
pub async fn forward(
    peer: &mut RouterClient<tonic::transport::Channel>,
    req: GenerateRequest,
) -> Result<tonic::Streaming<cgn_proto::v1::Token>> {
    let req_stream = futures::stream::iter(vec![req]);
    peer.generate(tonic::Request::new(req_stream))
        .await
        .map(|r| r.into_inner())
        .map_err(|s| Error::Unavailable(format!("federation forward: {s}")))
}
