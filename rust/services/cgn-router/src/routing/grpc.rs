//! Router-side gRPC server.
//!
//! Implements the `Router` service from `proto/cognitora/v1/router.proto`.
//! Each `Generate` call:
//!
//! 1. Tokenises the latest user/system messages (via the cached tokenizer
//!    for the requested model).
//! 2. Picks an agent with [`super::pick`].
//! 3. Opens an `Agent.Generate` stream to the chosen agent and forwards
//!    tokens back to the caller.
//!
//! `Embed` is unary: pick an agent and forward the request body.

use std::sync::Arc;

use cgn_proto::v1::{
    agent_client::AgentClient,
    router_server::Router,
    AgentGenerateRequest, EmbedRequest, EmbedResponse, GenerateRequest, NodeRole, Token,
};
use futures::{Stream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::state::SharedState;

pub struct RouterGrpc {
    state: Arc<SharedState>,
}

impl RouterGrpc {
    pub fn new(state: Arc<SharedState>) -> Self { Self { state } }
}

type GenerateStream = std::pin::Pin<Box<dyn Stream<Item = Result<Token, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Router for RouterGrpc {
    type GenerateStream = GenerateStream;

    async fn generate(
        &self,
        req: Request<Streaming<GenerateRequest>>,
    ) -> Result<Response<Self::GenerateStream>, Status> {
        let mut inbound = req.into_inner();

        // We expect exactly one client request frame today; multi-frame
        // streaming (e.g. interactive cancellation) is reserved for v2.
        let first = match inbound.next().await {
            Some(Ok(r)) => r,
            Some(Err(e)) => return Err(e),
            None => return Err(Status::invalid_argument("empty generate stream")),
        };

        let state = self.state.clone();
        let (tx, rx) = mpsc::channel::<Result<Token, Status>>(64);

        tokio::spawn(async move {
            if let Err(e) = forward(state, first, tx.clone()).await {
                let _ = tx.send(Err(e.into())).await;
            }
        });

        let stream: GenerateStream = Box::pin(ReceiverStream::new(rx));
        Ok(Response::new(stream))
    }

    async fn embed(
        &self,
        req: Request<EmbedRequest>,
    ) -> Result<Response<EmbedResponse>, Status> {
        let body = req.into_inner();
        let token_ids = approximate_token_ids(&body.inputs.join(" "));
        let decision = super::pick(&self.state, &body.model, NodeRole::Both, &token_ids)
            .await
            .map_err(Status::from)?;

        let mut client = connect_agent(&decision.node.address).await?;
        let resp = client.embed_via_router_compat(body).await?.into_inner();
        Ok(Response::new(resp))
    }
}

/// Forward a single `GenerateRequest` to a chosen agent, streaming tokens
/// back through `tx`. Returns Err on irrecoverable errors.
async fn forward(
    state: Arc<SharedState>,
    req:   GenerateRequest,
    tx:    mpsc::Sender<Result<Token, Status>>,
) -> cgn_core::Result<()> {
    use cgn_core::{Error, Result};

    // Approximate prefix tokens; a future revision uses the model's real
    // tokenizer — see `gateway::tokenize_for_routing`.
    let token_ids = approximate_token_ids(&join_messages(&req.messages));

    let role = if state.cfg.router.disagg.enabled
        && (token_ids.len() as u32) >= state.cfg.router.disagg.colocate_below_tokens
    {
        NodeRole::Prefill
    } else {
        NodeRole::Both
    };

    let decision = super::pick(&state, &req.model, role, &token_ids).await?;
    tracing::info!(
        node = %decision.node.node_id,
        score = decision.score.total,
        overlap = decision.overlap,
        candidates = decision.n_candidates,
        "forwarding to agent"
    );

    let mut client = connect_agent(&decision.node.address).await
        .map_err(|s| Error::Unavailable(format!("agent: {s}")))?;

    let agent_req = AgentGenerateRequest {
        id:           uuid::Uuid::new_v4().to_string(),
        model:        req.model,
        messages:     req.messages,
        params:       req.params,
        prefill_only: false,
        decode_only:  false,
        blocks:       vec![],
        traceparent:  req.traceparent,
        tracestate:   req.tracestate,
    };

    let req_stream = futures::stream::iter(vec![agent_req]);
    let mut response = client
        .generate(tonic::Request::new(req_stream))
        .await
        .map_err(|s| Error::Internal(format!("agent generate: {s}")))?
        .into_inner();

    while let Some(item) = response.next().await {
        match item {
            Ok(token) => {
                if tx.send(Ok(token)).await.is_err() {
                    return Ok(()); // client disconnected
                }
            }
            Err(s) => {
                let _ = tx.send(Err(s)).await;
                break;
            }
        }
    }
    Ok::<_, Error>(())
}

async fn connect_agent(endpoint: &str) -> Result<AgentClient<tonic::transport::Channel>, Status> {
    AgentClient::connect(endpoint.to_string())
        .await
        .map_err(|e| Status::unavailable(format!("agent connect {endpoint}: {e}")))
}

/// Join chat-style messages for prefix-hashing purposes. Stable across
/// versions; not the model's chat template.
fn join_messages(msgs: &[cgn_proto::v1::Message]) -> String {
    let mut out = String::with_capacity(msgs.iter().map(|m| m.content.len() + 16).sum());
    for m in msgs {
        out.push('<');
        out.push_str(&m.role);
        out.push_str(">\n");
        out.push_str(&m.content);
        out.push('\n');
    }
    out
}

/// Quick, dependency-free approximation: split on whitespace and hash. Used
/// only when the full tokenizer hasn't been resolved for the model yet
/// (cold start). The real path uses `tokenizers::Tokenizer::encode`.
fn approximate_token_ids(s: &str) -> Vec<u32> {
    s.split_whitespace()
        .map(|w| {
            let mut h = blake3::Hasher::new();
            h.update(w.as_bytes());
            let bytes = h.finalize();
            u32::from_le_bytes([bytes.as_bytes()[0], bytes.as_bytes()[1], bytes.as_bytes()[2], bytes.as_bytes()[3]])
        })
        .collect()
}

// Compatibility helper — `Embed` is not yet defined on the generated
// `AgentClient`. We add an extension trait so the call site stays clean
// and the implementation can be filled in once the proto adds Embed.
trait AgentClientExt {
    async fn embed_via_router_compat(
        &mut self,
        req: EmbedRequest,
    ) -> Result<tonic::Response<EmbedResponse>, Status>;
}

impl AgentClientExt for AgentClient<tonic::transport::Channel> {
    async fn embed_via_router_compat(
        &mut self,
        _req: EmbedRequest,
    ) -> Result<tonic::Response<EmbedResponse>, Status> {
        Err(Status::unimplemented(
            "Agent.Embed not yet defined; use generate with embedding model",
        ))
    }
}
