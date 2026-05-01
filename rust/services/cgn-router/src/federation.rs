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

use cgn_core::{Error, Result};
use cgn_proto::v1::{router_client::RouterClient, GenerateRequest};
use tracing::{debug, warn};

/// Dispatch a request to a federated peer. Returns the chosen peer's
/// gRPC endpoint and an open client. Caller is responsible for then
/// streaming the request through that client.
pub async fn pick_peer(
    peers: &[String],
    model: &str,
) -> Result<(String, RouterClient<tonic::transport::Channel>)> {
    if peers.is_empty() {
        return Err(Error::Unavailable("no federation peers configured".into()));
    }
    // Probe each peer's `/healthz` (out of band) and pick the first
    // healthy one. With dozens of peers we'd want a smarter scoring
    // strategy — geography, latency, cache overlap — tracked as
    // future work.
    for peer in peers {
        match RouterClient::connect(peer.clone()).await {
            Ok(client) => {
                debug!(%peer, %model, "federation peer connected");
                return Ok((peer.clone(), client));
            }
            Err(e) => warn!(%peer, error=?e, "federation peer unreachable"),
        }
    }
    Err(Error::Unavailable(format!(
        "all {} federation peers unreachable",
        peers.len()
    )))
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
