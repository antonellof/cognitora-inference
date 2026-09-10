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
    router_server::Router, AgentGenerateRequest, EmbedRequest, EmbedResponse, GenerateRequest,
    NodeRole, Token,
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
    pub fn new(state: Arc<SharedState>) -> Self {
        Self { state }
    }
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

    async fn embed(&self, req: Request<EmbedRequest>) -> Result<Response<EmbedResponse>, Status> {
        let body = req.into_inner();
        if body.inputs.is_empty() {
            return Err(Status::invalid_argument("inputs must be non-empty"));
        }
        let token_ids = super::prompt::approximate_token_ids(&body.inputs.join(" "));
        let decision = super::pick(&self.state, &body.model, NodeRole::Both, &token_ids)
            .await
            .map_err(Status::from)?;

        let mut client = self
            .state
            .connect_agent(&decision.node.address)
            .await
            .map_err(|e| Status::unavailable(format!("agent: {e}")))?;
        let resp = client.embed(body).await?.into_inner();
        Ok(Response::new(resp))
    }
}

/// Forward a single `GenerateRequest` to a chosen agent, streaming tokens
/// back through `tx`. Returns Err on irrecoverable errors.
async fn forward(
    state: Arc<SharedState>,
    req: GenerateRequest,
    tx: mpsc::Sender<Result<Token, Status>>,
) -> cgn_core::Result<()> {
    use cgn_core::Error;

    // Approximate prefix tokens; a future revision uses the model's real
    // tokenizer. Shared with the HTTP gateway so both surfaces compute
    // identical prefix hashes for the same prompt.
    let token_ids =
        super::prompt::approximate_token_ids(&super::prompt::join_messages(&req.messages));

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

    let mut client = state.connect_agent(&decision.node.address).await?;

    let agent_req = AgentGenerateRequest {
        id: uuid::Uuid::new_v4().to_string(),
        model: req.model,
        messages: req.messages,
        params: req.params,
        prefill_only: false,
        decode_only: false,
        blocks: vec![],
        traceparent: req.traceparent,
        tracestate: req.tracestate,
        extensions_json: req.extensions_json,
        digests: decision.digests.iter().map(|d| d.to_vec()).collect(),
    };

    let req_stream = futures::stream::iter(vec![agent_req]);
    let mut response = client
        .generate(tonic::Request::new(req_stream))
        .await
        .map_err(|s| Error::Internal(format!("agent generate: {s}")))?
        .into_inner();

    // Optimistic prefix announcement: the chosen node's engine will hold
    // the KV for this prompt's prefix once the prefill completes, so
    // record it now (TTL-bounded) for KV-aware routing of follow-ups.
    state
        .prefix
        .insert_many(&decision.digests, &decision.node.node_id);

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
