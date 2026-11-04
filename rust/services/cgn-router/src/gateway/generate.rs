//! The generation path shared by every HTTP surface.
//!
//! [`open`] admits a request, routes it and waits for the first token, so
//! failures surface while the handler can still return a real HTTP status,
//! and a node that dies before producing anything is retried elsewhere.
//! Past the first token a stream can no longer be replayed invisibly, so
//! later errors end it with an SSE error event.

use std::sync::{Arc, LazyLock};
use std::time::Instant;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use cgn_proto::v1::{GenerateRequest, NodeRole, Token};
use cgn_telemetry::counter_vec;
use cgn_telemetry::prometheus::IntCounterVec;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::admission::Permit;
use crate::cascade::{Cascade, StepOutcome};
use crate::routing::{self, RouteKey};
use crate::state::{NodeGuard, SharedState};

use super::authz::Caller;
use super::metrics::{CHAT_COMPLETION_TOKENS, CHAT_LATENCY, CHAT_REQUESTS, CHAT_TTFT};
use super::sse::{self, SseEvent};
use super::types::{ChatChunk, ChatChunkChoice, ChatDelta, TextChoice, TextCompletion, Usage};

/// Maximum dispatch attempts per request.
const MAX_DISPATCH_ATTEMPTS: usize = 3;

static CASCADE_CONFIDENCE: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_cascade_confidence_total",
        "Cascade gating decisions by confidence source (logprobs or heuristic).",
        &["source"]
    )
});

type TokenStream = BoxStream<'static, Result<Token, tonic::Status>>;

/// An admitted request with its first token in hand. Holds the admission
/// slot and the node's in-flight count until dropped.
pub struct TokenFlow {
    first: Option<Token>,
    rest: TokenStream,
    /// Router-side prompt estimate, used when the engine reports no usage.
    prompt_estimate: u32,
    first_token_at: Instant,
    started: Instant,
    _permit: Permit,
    _node: Option<NodeGuard>,
}

impl TokenFlow {
    async fn next(&mut self) -> Option<Result<Token, tonic::Status>> {
        match self.first.take() {
            Some(t) => Some(Ok(t)),
            None => self.rest.next().await,
        }
    }
}

/// Admit, route and wait for the first token.
pub async fn open(state: &Arc<SharedState>, req: &GenerateRequest) -> cgn_core::Result<TokenFlow> {
    let started = Instant::now();
    let text = routing::prompt::request_text(&req.prompt, &req.messages);
    let token_ids = state.tokenizers.token_ids(&req.model, &text).await;
    let permit =
        crate::admission::admit_request(state, &req.model, token_ids.len() as u32, &req.tenant)
            .await?;

    let salt = routing::prefix_salt(state, &req.model, &req.tenant);
    let key = RouteKey {
        model: &req.model,
        salt: &salt,
        session: (!req.session.is_empty()).then_some(req.session.as_str()),
        token_ids: &token_ids,
    };

    let mut exclude: Vec<String> = Vec::new();
    let mut last_err = None;
    for attempt in 1..=MAX_DISPATCH_ATTEMPTS {
        match dispatch_once(state, req, &key, &exclude).await {
            Ok((mut stream, node_id, address, guard)) => {
                match first_token(state, &mut stream).await {
                    FirstToken::Got(first) => {
                        return Ok(TokenFlow {
                            first: Some(first),
                            rest: stream,
                            prompt_estimate: token_ids.len() as u32,
                            first_token_at: Instant::now(),
                            started,
                            _permit: permit,
                            _node: Some(guard),
                        });
                    }
                    FirstToken::Fatal(e) => return Err(e),
                    FirstToken::Retry(e) => {
                        warn!(%node_id, attempt, error = ?e, "no first token; trying next node");
                        state.forget_agent(&address);
                        exclude.push(node_id);
                        last_err = Some(e);
                    }
                }
            }
            Err((None, e)) if matches!(e, cgn_core::Error::Unavailable(_)) => {
                // Nothing local left: try peer clusters, then report the
                // failure that matters most.
                let fed = &state.cfg.router.federation;
                if fed.enabled && !fed.peers.is_empty() {
                    match federate(state, req).await {
                        Ok(mut stream) => {
                            if let FirstToken::Got(first) = first_token(state, &mut stream).await {
                                return Ok(TokenFlow {
                                    first: Some(first),
                                    rest: stream,
                                    prompt_estimate: token_ids.len() as u32,
                                    first_token_at: Instant::now(),
                                    started,
                                    _permit: permit,
                                    _node: None,
                                });
                            }
                        }
                        Err(fe) => warn!(error = ?fe, "federation fallback failed"),
                    }
                }
                return Err(last_err.unwrap_or(e));
            }
            Err((Some(node), e)) if retryable(&e) => {
                warn!(%node, attempt, error = ?e, "dispatch failed; retrying on next-best node");
                exclude.push(node);
                last_err = Some(e);
            }
            Err((_, e)) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| cgn_core::Error::Unavailable("dispatch exhausted".into())))
}

fn retryable(e: &cgn_core::Error) -> bool {
    matches!(
        e,
        cgn_core::Error::Unavailable(_) | cgn_core::Error::Internal(_)
    )
}

enum FirstToken {
    Got(Token),
    Retry(cgn_core::Error),
    Fatal(cgn_core::Error),
}

/// Wait for the first token. Failures here are invisible to the client,
/// so anything that isn't the caller's fault can go to another node.
async fn first_token(state: &SharedState, stream: &mut TokenStream) -> FirstToken {
    let wait = state.cfg.router.first_token_timeout;
    match tokio::time::timeout(wait, stream.next()).await {
        Err(_) => FirstToken::Retry(cgn_core::Error::Unavailable(format!(
            "no token within {wait:?}"
        ))),
        Ok(None) => FirstToken::Retry(cgn_core::Error::Internal(
            "engine stream ended before the first token".into(),
        )),
        Ok(Some(Ok(t))) => FirstToken::Got(t),
        Ok(Some(Err(s))) => match s.code() {
            tonic::Code::InvalidArgument
            | tonic::Code::NotFound
            | tonic::Code::PermissionDenied
            | tonic::Code::Unauthenticated
            | tonic::Code::FailedPrecondition
            | tonic::Code::OutOfRange => {
                FirstToken::Fatal(cgn_core::Error::InvalidArgument(s.message().to_string()))
            }
            _ => FirstToken::Retry(cgn_core::Error::Internal(s.message().to_string())),
        },
    }
}

/// Forward to the best federation peer.
async fn federate(
    state: &Arc<SharedState>,
    req: &GenerateRequest,
) -> cgn_core::Result<TokenStream> {
    let fed = &state.cfg.router.federation;
    let (peer, mut client) = crate::federation::pick_peer(&fed.peers, &req.model).await?;
    let stream = crate::federation::forward(&mut client, req.clone()).await?;
    info!(%peer, model = %req.model, "no local node; request federated to peer cluster");
    super::metrics::FEDERATION_FORWARDS
        .with_label_values(&[&req.model, &peer])
        .inc();
    Ok(Box::pin(stream))
}

type Dispatched = (TokenStream, String, String, NodeGuard);

/// One attempt: plan, pick, prefill if split, then decode. On failure
/// returns the node to exclude, or `None` if routing itself found nothing.
async fn dispatch_once(
    state: &Arc<SharedState>,
    req: &GenerateRequest,
    key: &RouteKey<'_>,
    exclude: &[String],
) -> Result<Dispatched, (Option<String>, cgn_core::Error)> {
    use crate::disagg::Plan;
    use cgn_proto::v1::AgentGenerateRequest;

    let disagg = &state.cfg.router.disagg;
    let plan = crate::disagg::plan(
        disagg.enabled,
        disagg.colocate_below_tokens,
        key.token_ids.len() as u32,
    );
    let (prefill_decision, decode_decision) = match plan {
        Plan::Colocate => {
            let d = routing::pick_excluding(state, key, NodeRole::Both, exclude)
                .await
                .map_err(|e| (None, e))?;
            (d.clone(), d)
        }
        Plan::Split {
            prefill_role,
            decode_role,
        } => {
            let (p, d) = routing::pick_pair(state, key, prefill_role, decode_role, exclude)
                .await
                .map_err(|e| (None, e))?;
            info!(
                prefill = %p.node.node_id,
                decode  = %d.node.node_id,
                "disagg pair selected"
            );
            (p, d)
        }
    };

    info!(
        node = %decode_decision.node.node_id,
        score = decode_decision.score.total,
        overlap = decode_decision.overlap,
        "openai → routing decision"
    );

    let decode_node_id = decode_decision.node.node_id.clone();
    match crate::deadline::check(state, req, &decode_decision.node) {
        crate::deadline::AdmissionOutcome::Admit => {}
        crate::deadline::AdmissionOutcome::DeadlineExceeded { .. } => {
            crate::admission::record_rejection(&req.model, "ttft_violation");
            return Err((
                Some(decode_node_id),
                cgn_core::Error::Unavailable("deadline exceeded given current queue depth".into()),
            ));
        }
        crate::deadline::AdmissionOutcome::QueueFull => {
            crate::admission::record_rejection(&req.model, "queue_full");
            return Err((
                Some(decode_node_id),
                cgn_core::Error::Unavailable("admission queue full".into()),
            ));
        }
    }

    let guard = state.track_node(&decode_node_id);

    // Phase 1: prefill (only when split *and* prefill node ≠ decode node).
    let prefill_blocks: Vec<Vec<u8>> =
        if prefill_decision.node.node_id != decode_decision.node.node_id {
            run_prefill(state.clone(), &prefill_decision.node.address, req)
                .await
                .unwrap_or_default()
        } else {
            vec![]
        };

    // Phase 2: decode. Pass the prefill block list so the engine can
    // skip the first forward pass.
    let address = decode_decision.node.address.clone();
    let mut client = state
        .connect_agent(&address)
        .await
        .map_err(|e| (Some(decode_node_id.clone()), e))?;

    let agent_req = AgentGenerateRequest {
        id: uuid::Uuid::new_v4().to_string(),
        model: req.model.clone(),
        messages: req.messages.clone(),
        params: req.params.clone(),
        prefill_only: false,
        decode_only: !prefill_blocks.is_empty(),
        blocks: prefill_blocks,
        traceparent: req.traceparent.clone(),
        tracestate: req.tracestate.clone(),
        extensions_json: req.extensions_json.clone(),
        // The agent publishes these as *confirmed* KV claims to etcd
        // after the generation completes successfully.
        digests: decode_decision.digests.iter().map(|d| d.to_vec()).collect(),
        prompt: req.prompt.clone(),
    };
    let req_stream = futures::stream::iter(vec![agent_req]);
    let response = client
        .generate(tonic::Request::new(req_stream))
        .await
        .map_err(|s| {
            state.forget_agent(&address);
            (
                Some(decode_node_id.clone()),
                cgn_core::Error::Internal(format!("agent generate: {s}")),
            )
        })?
        .into_inner();

    // Optimistic prefix announcement: both the prefill and decode nodes
    // end up holding this prompt's prefix KV, so record them (TTL-bounded)
    // for KV-aware routing of follow-up turns. Only after a successful
    // dispatch — failed nodes must not accrue prefix claims.
    state
        .prefix
        .insert_many(&decode_decision.digests, &decode_decision.node.node_id);
    if prefill_decision.node.node_id != decode_decision.node.node_id {
        state
            .prefix
            .insert_many(&prefill_decision.digests, &prefill_decision.node.node_id);
    }

    Ok((Box::pin(response), decode_node_id, address, guard))
}

/// Issue a prefill-only request to the prefill agent and collect the
/// returned block list. On any error returns an empty vec and the
/// caller falls back to colocate execution.
async fn run_prefill(
    state: Arc<SharedState>,
    address: &str,
    req: &GenerateRequest,
) -> Option<Vec<Vec<u8>>> {
    use cgn_proto::v1::AgentGenerateRequest;

    let mut client = state.connect_agent(address).await.ok()?;
    let prefill_req = AgentGenerateRequest {
        id: uuid::Uuid::new_v4().to_string(),
        model: req.model.clone(),
        messages: req.messages.clone(),
        params: req.params.clone(),
        prefill_only: true,
        decode_only: false,
        blocks: vec![],
        traceparent: req.traceparent.clone(),
        tracestate: req.tracestate.clone(),
        extensions_json: req.extensions_json.clone(),
        digests: vec![],
        prompt: req.prompt.clone(),
    };
    let req_stream = futures::stream::iter(vec![prefill_req]);
    let mut stream = client
        .generate(tonic::Request::new(req_stream))
        .await
        .ok()?
        .into_inner();
    // The prefill agent terminates the stream after publishing the KV
    // handoff metadata. We don't currently surface that metadata back —
    // a future revision will lift the handoff message into a
    // side-channel so the router can drive the QUIC push between agents.
    let _ = stream.next().await;
    Some(vec![])
}

/// A finished, fully buffered generation.
#[derive(Debug, Default)]
pub struct Generated {
    pub text: String,
    pub finish: String,
    pub tool_calls: Option<serde_json::Value>,
    pub logprobs: Option<serde_json::Value>,
    pub usage: Usage,
    /// Mean token logprob, when the engine reported any.
    pub mean_logprob: Option<f32>,
}

/// Running totals over a token stream.
#[derive(Default)]
struct Tally {
    chunks: u32,
    logprob_sum: f64,
    logprob_n: u32,
    prompt_tokens: u32,
    completion_tokens: u32,
}

impl Tally {
    fn observe(&mut self, t: &Token) {
        if !t.text.is_empty() || !t.tool_calls_json.is_empty() {
            self.chunks += 1;
        }
        if t.logprob != 0.0 {
            self.logprob_sum += t.logprob as f64;
            self.logprob_n += 1;
        }
        self.prompt_tokens = self.prompt_tokens.max(t.prompt_tokens);
        self.completion_tokens = self.completion_tokens.max(t.completion_tokens);
    }

    /// Engine-reported usage, or the router's estimate where it has none.
    fn usage(&self, prompt_estimate: u32) -> Usage {
        let prompt_tokens = if self.prompt_tokens > 0 {
            self.prompt_tokens
        } else {
            prompt_estimate
        };
        let completion_tokens = if self.completion_tokens > 0 {
            self.completion_tokens
        } else {
            self.chunks
        };
        Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    }

    fn mean_logprob(&self) -> Option<f32> {
        (self.logprob_n > 0).then(|| (self.logprob_sum / self.logprob_n as f64) as f32)
    }
}

/// Drain a flow into a [`Generated`].
pub async fn collect(mut flow: TokenFlow) -> cgn_core::Result<Generated> {
    let mut out = Generated {
        finish: "stop".into(),
        ..Generated::default()
    };
    let mut tally = Tally::default();
    let mut tools = ToolCallAggregator::default();
    let mut logprobs: Vec<serde_json::Value> = Vec::new();
    while let Some(item) = flow.next().await {
        let t = item.map_err(|e| cgn_core::Error::Internal(format!("stream: {e}")))?;
        tally.observe(&t);
        out.text.push_str(&t.text);
        if !t.tool_calls_json.is_empty() {
            if let Ok(v) = serde_json::from_str(&t.tool_calls_json) {
                tools.push(&v);
            }
        }
        if !t.logprobs_json.is_empty() {
            if let Ok(serde_json::Value::Object(o)) = serde_json::from_str(&t.logprobs_json) {
                if let Some(serde_json::Value::Array(items)) = o.get("content") {
                    logprobs.extend(items.iter().cloned());
                }
            }
        }
        if !t.finish.is_empty() {
            out.finish = t.finish;
        }
    }
    out.tool_calls = tools.finish();
    if !logprobs.is_empty() {
        out.logprobs = Some(json!({ "content": logprobs }));
    }
    out.usage = tally.usage(flow.prompt_estimate);
    out.mean_logprob = tally.mean_logprob();
    info!(tokens = out.usage.completion_tokens, "completion finished");
    Ok(out)
}

/// How a cascade (or plain request) resolved.
pub enum Resolved {
    Live(TokenFlow),
    Done(Generated),
}

/// Length-based stand-in for engines that can't report logprobs. It is a
/// guess; `cgn_router_cascade_confidence_total{source="heuristic"}` shows
/// how often a deployment relies on it.
fn heuristic_logprob(tokens: u32) -> f32 {
    -1.0 / ((tokens as f32).max(1.0)).ln().max(0.5)
}

/// Run the cascade's early steps, returning the accepted answer or a live
/// flow from the final model. Early steps run buffered because confidence
/// needs the whole output; nothing reaches the client until one is chosen.
pub async fn resolve_cascade(
    state: &Arc<SharedState>,
    proto: &GenerateRequest,
    casc: &Cascade,
) -> cgn_core::Result<Resolved> {
    let (last, early) = casc
        .steps
        .split_last()
        .expect("cascade has at least one step");
    for step in early {
        let mut step_proto = proto.clone();
        step_proto.model = step.clone();
        if let Some(p) = step_proto.params.as_mut() {
            p.logprobs = true;
        }
        let generated = match open(state, &step_proto).await {
            Ok(flow) => collect(flow).await,
            Err(e) => Err(e),
        };
        match generated {
            Ok(g) => {
                let (logprob, source) = match g.mean_logprob {
                    Some(lp) => (lp, "logprobs"),
                    None => (heuristic_logprob(g.usage.completion_tokens), "heuristic"),
                };
                CASCADE_CONFIDENCE.with_label_values(&[source]).inc();
                let outcome = StepOutcome {
                    logprob,
                    tokens: g.usage.completion_tokens,
                    ..StepOutcome::default()
                };
                if !casc.should_escalate(&outcome) {
                    info!(step = %step, logprob, source, "cascade: early step accepted");
                    return Ok(Resolved::Done(g));
                }
                tracing::debug!(step = %step, logprob, source, "cascade escalating");
            }
            Err(e) => warn!(step = %step, error = ?e, "cascade step failed; escalating"),
        }
    }
    let mut step_proto = proto.clone();
    step_proto.model = last.clone();
    Ok(Resolved::Live(open(state, &step_proto).await?))
}

/// Which OpenAI response shape to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Chat,
    Text,
}

/// Everything a response needs besides the tokens.
#[derive(Debug, Clone)]
pub struct ResponseCtx {
    pub id: String,
    pub created: i64,
    pub model: String,
    pub surface: Surface,
    pub include_usage: bool,
    pub caller: Caller,
}

impl ResponseCtx {
    fn chunk(
        &self,
        role: Option<&str>,
        text: Option<String>,
        tool_calls: Option<serde_json::Value>,
        finish: Option<String>,
        logprobs: Option<serde_json::Value>,
    ) -> String {
        match self.surface {
            Surface::Chat => serde_json::to_string(&ChatChunk {
                id: self.id.clone(),
                object: "chat.completion.chunk",
                created: self.created,
                model: self.model.clone(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatDelta {
                        role: role.map(str::to_string),
                        content: text,
                        tool_calls,
                    },
                    finish_reason: finish,
                    logprobs,
                }],
                usage: None,
            }),
            Surface::Text => serde_json::to_string(&TextCompletion {
                id: self.id.clone(),
                object: "text_completion",
                created: self.created,
                model: self.model.clone(),
                choices: vec![TextChoice {
                    index: 0,
                    text: text.unwrap_or_default(),
                    finish_reason: finish,
                    logprobs,
                }],
                usage: None,
            }),
        }
        .expect("chunk serializes")
    }

    /// `stream_options.include_usage`: a last chunk with empty `choices`.
    fn usage_chunk(&self, usage: Usage) -> String {
        match self.surface {
            Surface::Chat => serde_json::to_string(&ChatChunk {
                id: self.id.clone(),
                object: "chat.completion.chunk",
                created: self.created,
                model: self.model.clone(),
                choices: vec![],
                usage: Some(usage),
            }),
            Surface::Text => serde_json::to_string(&TextCompletion {
                id: self.id.clone(),
                object: "text_completion",
                created: self.created,
                model: self.model.clone(),
                choices: vec![],
                usage: Some(usage),
            }),
        }
        .expect("usage chunk serializes")
    }
}

/// Serve a resolved request as SSE.
pub fn stream_response(
    state: Arc<SharedState>,
    resolved: Resolved,
    ctx: ResponseCtx,
    started: Instant,
) -> Response {
    let (tx, rx) = mpsc::channel::<SseEvent>(64);
    tokio::spawn(async move {
        let model = ctx.model.clone();
        let outcome = match resolved {
            Resolved::Live(flow) => pump(&state, flow, &tx, &ctx).await,
            Resolved::Done(g) => emit_generated(&state, g, &tx, &ctx).await,
        };
        let status = if outcome.is_ok() { "200" } else { "5xx" };
        CHAT_REQUESTS.with_label_values(&[&model, status]).inc();
        CHAT_LATENCY
            .with_label_values(&[&model, "true"])
            .observe(started.elapsed().as_secs_f64());
    });
    sse::into_response(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// Forward tokens as they arrive, then usage. A client disconnect drops
/// the flow, which cancels the upstream gRPC stream and the engine call.
async fn pump(
    state: &SharedState,
    mut flow: TokenFlow,
    tx: &mpsc::Sender<SseEvent>,
    ctx: &ResponseCtx,
) -> Result<(), ()> {
    CHAT_TTFT
        .with_label_values(&[&ctx.model])
        .observe((flow.first_token_at - flow.started).as_secs_f64());
    if ctx.surface == Surface::Chat {
        let role = ctx.chunk(Some("assistant"), None, None, None, None);
        if tx.send(SseEvent::Data(role)).await.is_err() {
            return Ok(());
        }
    }
    let mut tally = Tally::default();
    let mut result = Ok(());
    while let Some(item) = flow.next().await {
        match item {
            Ok(t) => {
                tally.observe(&t);
                let has_payload =
                    !t.text.is_empty() || !t.tool_calls_json.is_empty() || !t.finish.is_empty();
                if !has_payload {
                    continue; // usage-only terminal token
                }
                let text = (!t.text.is_empty()).then(|| t.text.clone());
                let tools = serde_json::from_str(&t.tool_calls_json).ok();
                let finish = (!t.finish.is_empty()).then(|| t.finish.clone());
                let logprobs = serde_json::from_str(&t.logprobs_json).ok();
                let chunk = ctx.chunk(None, text, tools, finish, logprobs);
                if tx.send(SseEvent::Data(chunk)).await.is_err() {
                    break; // client disconnected
                }
            }
            Err(e) => {
                warn!(error = %e, "stream failed after first token");
                let body = json!({
                    "error": {
                        "message": e.message(),
                        "type": "server_error",
                        "code": format!("{:?}", e.code()),
                    }
                });
                let _ = tx.send(SseEvent::Fail(body.to_string())).await;
                result = Err(());
                break;
            }
        }
    }
    let usage = tally.usage(flow.prompt_estimate);
    if result.is_ok() && ctx.include_usage {
        let _ = tx.send(SseEvent::Data(ctx.usage_chunk(usage))).await;
    }
    settle(state, ctx, usage);
    result
}

/// Serve an already-buffered answer (accepted cascade step) as SSE.
async fn emit_generated(
    state: &SharedState,
    g: Generated,
    tx: &mpsc::Sender<SseEvent>,
    ctx: &ResponseCtx,
) -> Result<(), ()> {
    if ctx.surface == Surface::Chat {
        let role = ctx.chunk(Some("assistant"), None, None, None, None);
        let _ = tx.send(SseEvent::Data(role)).await;
    }
    let body = ctx.chunk(None, Some(g.text), None, None, None);
    let _ = tx.send(SseEvent::Data(body)).await;
    let end = ctx.chunk(None, None, None, Some(g.finish), None);
    let _ = tx.send(SseEvent::Data(end)).await;
    if ctx.include_usage {
        let _ = tx.send(SseEvent::Data(ctx.usage_chunk(g.usage))).await;
    }
    settle(state, ctx, g.usage);
    Ok(())
}

/// Record a finished request's tokens in metrics and the ledger.
pub fn settle(state: &SharedState, ctx: &ResponseCtx, usage: Usage) {
    CHAT_COMPLETION_TOKENS
        .with_label_values(&[&ctx.model])
        .inc_by(usage.completion_tokens as u64);
    ctx.caller
        .charge(state, usage.prompt_tokens, usage.completion_tokens);
}

/// Client hints carried on headers.
pub fn apply_headers(proto: &mut GenerateRequest, headers: &HeaderMap) {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    proto.traceparent = get("traceparent").unwrap_or_default();
    proto.tracestate = get("tracestate").unwrap_or_default();
    if let Some(ms) = get("x-cgn-deadline-ms").and_then(|v| v.parse().ok()) {
        proto.deadline_ms = ms;
    }
    if let Some(session) = get("x-session-id") {
        proto.session = session;
    }
}

/// OpenAI-style error body with the matching HTTP status.
pub fn error_json(e: &cgn_core::Error) -> Response {
    let (status, code) = match e {
        cgn_core::Error::InvalidArgument(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
        cgn_core::Error::NotFound(_) => (StatusCode::NOT_FOUND, "not_found_error"),
        cgn_core::Error::Unavailable(msg)
            if msg.contains("admission queue full") || msg.contains("deadline exceeded") =>
        {
            (StatusCode::TOO_MANY_REQUESTS, "server_error")
        }
        cgn_core::Error::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "server_error"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "server_error"),
    };
    let mut resp = (
        status,
        Json(json!({
            "error": {
                "message": e.to_string(),
                "type": code,
                "code": null,
            }
        })),
    )
        .into_response();
    if matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
    ) {
        resp.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from_static("1"),
        );
    }
    resp
}

/// Merges streaming `delta.tool_calls` fragments into the final
/// `message.tool_calls` array, keyed by each fragment's `index`.
#[derive(Default)]
struct ToolCallAggregator {
    calls: std::collections::BTreeMap<u64, serde_json::Value>,
}
impl ToolCallAggregator {
    fn push(&mut self, delta: &serde_json::Value) {
        let Some(entries) = delta.as_array() else {
            return;
        };
        for e in entries {
            let idx = e.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
            let slot = self.calls.entry(idx).or_insert_with(|| {
                serde_json::json!({
                    "id": "",
                    "type": "function",
                    "function": { "name": "", "arguments": "" }
                })
            });
            if let Some(id) = e.get("id").and_then(|v| v.as_str()) {
                slot["id"] = id.into();
            }
            if let Some(t) = e.get("type").and_then(|v| v.as_str()) {
                slot["type"] = t.into();
            }
            if let Some(f) = e.get("function") {
                if let Some(name) = f.get("name").and_then(|v| v.as_str()) {
                    slot["function"]["name"] = name.into();
                }
                if let Some(args) = f.get("arguments").and_then(|v| v.as_str()) {
                    let existing = slot["function"]["arguments"].as_str().unwrap_or("");
                    slot["function"]["arguments"] = format!("{existing}{args}").into();
                }
            }
        }
    }

    fn finish(self) -> Option<serde_json::Value> {
        if self.calls.is_empty() {
            None
        } else {
            Some(serde_json::Value::Array(self.calls.into_values().collect()))
        }
    }
}
