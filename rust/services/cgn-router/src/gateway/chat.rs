//! `/v1/chat/completions` and `/v1/completions` handlers.
//!
//! Translates OpenAI's HTTP request shape into a `cognitora.v1` proto
//! `GenerateRequest`, invokes the routing logic in-process, and either:
//!
//! * Streams Server-Sent Events back to the client (`stream: true`).
//! * Buffers tokens and returns a single JSON body (`stream: false`).

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use cgn_proto::v1::{GenerateRequest, Message as PMessage, NodeRole, SamplingParams};
use futures::StreamExt;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};

use crate::cascade::{Cascade, StepOutcome};
use crate::routing;
use crate::state::SharedState;

use super::metrics::{CHAT_COMPLETION_TOKENS, CHAT_LATENCY, CHAT_REQUESTS};
use super::sse;
use super::types::{
    ChatChoice, ChatChunk, ChatChunkChoice, ChatDelta, ChatMessage, ChatRequest, ChatResponse,
    Usage,
};

pub async fn completions(
    State(state): State<Arc<SharedState>>,
    Json(req): Json<ChatRequest>,
) -> Response {
    let stream_mode = req.stream.unwrap_or(false);
    let id = format!(
        "chatcmpl-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..16]
    );
    let created = chrono::Utc::now().timestamp();
    let model = req.model.clone();
    let started = std::time::Instant::now();
    let stream_label = if stream_mode { "true" } else { "false" };

    // Build the proto request once.
    let proto_req = build_proto_request(&req);

    if stream_mode {
        let (tx, rx) = mpsc::channel::<String>(64);
        let id_for_task = id.clone();
        let model_for_task = model.clone();
        let state_clone = state.clone();
        let metric_model = model.clone();
        let started_for_metric = started;
        let casc = Cascade::from_config(&state.cfg, &model, &[]);
        tokio::spawn(async move {
            let outcome = match casc {
                Some(c) => {
                    stream_run_cascade(
                        state_clone,
                        proto_req,
                        c,
                        tx.clone(),
                        id_for_task,
                        model_for_task,
                        created,
                    )
                    .await
                }
                None => {
                    stream_run(
                        state_clone,
                        proto_req,
                        tx.clone(),
                        id_for_task,
                        model_for_task,
                        created,
                    )
                    .await
                }
            };
            let status = if outcome.is_ok() { "200" } else { "5xx" };
            CHAT_REQUESTS
                .with_label_values(&[&metric_model, status])
                .inc();
            CHAT_LATENCY
                .with_label_values(&[&metric_model, "true"])
                .observe(started_for_metric.elapsed().as_secs_f64());
            if let Err(e) = outcome {
                error!(error=?e, "stream_run error");
            }
        });
        let stream = ReceiverStream::new(rx);
        return sse::into_response(stream);
    }

    // Buffered path: the cascade evaluates the complete response's
    // confidence before deciding whether to escalate.
    let casc = Cascade::from_config(&state.cfg, &model, &[]);
    let result = match casc {
        Some(c) => buffered_with_cascade(state.clone(), proto_req, c).await,
        None => buffered_run(state.clone(), proto_req)
            .await
            .map(|(text, n, finish)| (text, n, finish, model.clone())),
    };

    let dt = started.elapsed().as_secs_f64();
    CHAT_LATENCY
        .with_label_values(&[&model, stream_label])
        .observe(dt);

    match result {
        Ok((text, completion_tokens, finish, used_model)) => {
            CHAT_REQUESTS.with_label_values(&[&used_model, "200"]).inc();
            CHAT_COMPLETION_TOKENS
                .with_label_values(&[&used_model])
                .inc_by(completion_tokens as u64);
            let resp = ChatResponse {
                id,
                object: "chat.completion",
                created,
                model: used_model,
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessage {
                        role: "assistant".into(),
                        content: text,
                        name: None,
                    },
                    finish_reason: finish,
                }],
                usage: Usage {
                    prompt_tokens: 0,
                    completion_tokens,
                    total_tokens: completion_tokens,
                },
            };
            Json(resp).into_response()
        }
        Err(e) => {
            CHAT_REQUESTS.with_label_values(&[&model, "5xx"]).inc();
            warn!(error=?e, "completion failed");
            error_json(&e)
        }
    }
}

/// Run a buffered completion through a model cascade. Each step
/// re-routes through `routing::pick` so we always pick the best node
/// for that particular model.
async fn buffered_with_cascade(
    state: Arc<SharedState>,
    proto: GenerateRequest,
    casc: Cascade,
) -> cgn_core::Result<(String, u32, String, String)> {
    let result = casc
        .run(|model| {
            let state = state.clone();
            let mut step_proto = proto.clone();
            step_proto.model = model.to_string();
            async move {
                match buffered_run(state, step_proto).await {
                    Ok((text, n, finish)) => StepOutcome {
                        logprob: heuristic_logprob(n),
                        text,
                        tokens: n,
                        finish,
                    },
                    Err(e) => {
                        tracing::warn!(error=?e, "cascade step failed; escalating");
                        StepOutcome::default()
                    }
                }
            }
        })
        .await;

    info!(
        used = %result.model_used,
        attempts = result.steps_attempted.len(),
        tokens = result.outcome.tokens,
        "cascade complete"
    );
    Ok((
        result.outcome.text,
        result.outcome.tokens,
        result.outcome.finish,
        result.model_used,
    ))
}

fn build_proto_request(r: &ChatRequest) -> GenerateRequest {
    let messages: Vec<PMessage> = r
        .messages
        .iter()
        .map(|m| PMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            name: m.name.clone().unwrap_or_default(),
        })
        .collect();

    let stops = r
        .stop
        .as_ref()
        .cloned()
        .map(|s| s.into_vec())
        .unwrap_or_default();

    GenerateRequest {
        model: r.model.clone(),
        messages,
        params: Some(SamplingParams {
            temperature: r.temperature.unwrap_or(1.0),
            top_p: r.top_p.unwrap_or(1.0),
            top_k: 0,
            max_tokens: r.max_tokens.unwrap_or(0),
            stop: stops,
            logprobs: false,
            seed: r.seed.unwrap_or(0),
            frequency_penalty: r.frequency_penalty.unwrap_or(0.0),
            presence_penalty: r.presence_penalty.unwrap_or(0.0),
            repetition_penalty: 1.0,
        }),
        tenant: r.user.clone().unwrap_or_default(),
        prefix_hash: vec![],
        stream: r.stream.unwrap_or(false),
        cascade: vec![],
        traceparent: String::new(),
        tracestate: String::new(),
        deadline_ms: 0,
    }
}

/// Without engine-side logprobs, approximate confidence with a
/// length-based heuristic: longer responses imply higher confidence.
/// Real impl plugs in the engine's mean-logprob output once exposed.
fn heuristic_logprob(tokens: u32) -> f32 {
    -1.0 / ((tokens as f32).max(1.0)).ln().max(0.5)
}

/// Build one SSE chunk JSON string.
fn chunk_json(
    id: &str,
    created: i64,
    model: &str,
    role: Option<&str>,
    content: Option<&str>,
    finish: Option<&str>,
) -> String {
    let chunk = ChatChunk {
        id: id.to_string(),
        object: "chat.completion.chunk",
        created,
        model: model.to_string(),
        choices: vec![ChatChunkChoice {
            index: 0,
            delta: ChatDelta {
                role: role.map(str::to_string),
                content: content.map(str::to_string),
            },
            finish_reason: finish.map(str::to_string),
        }],
    };
    serde_json::to_string(&chunk).unwrap()
}

/// Streaming path with a model cascade (SLM → … → LLM).
///
/// Steps before the last are executed *buffered* — confidence gating
/// needs the complete output — and a passing step's text is emitted as
/// a single content chunk. If every early step escalates, the final
/// model streams live token-by-token, so the worst case degrades to
/// exactly the non-cascade streaming behavior (plus the SLM detour).
async fn stream_run_cascade(
    state: Arc<SharedState>,
    proto: GenerateRequest,
    casc: Cascade,
    tx: mpsc::Sender<String>,
    id: String,
    model: String,
    created: i64,
) -> cgn_core::Result<()> {
    if casc.steps.is_empty() {
        return stream_run(state, proto, tx, id, model, created).await;
    }
    // Role chunk first, tagged with the originally requested model —
    // per-step model substitution is an internal routing detail.
    let _ = tx
        .send(chunk_json(
            &id,
            created,
            &model,
            Some("assistant"),
            None,
            None,
        ))
        .await;

    let n = casc.steps.len();
    for (i, step) in casc.steps.iter().enumerate() {
        let mut step_proto = proto.clone();
        step_proto.model = step.clone();

        if i + 1 == n {
            // Final step: stream live.
            info!(step = %step, "cascade streaming final step");
            return stream_tokens(state, step_proto, tx, id, model, created).await;
        }

        match buffered_run(state.clone(), step_proto).await {
            Ok((text, tokens, finish)) => {
                let outcome = StepOutcome {
                    logprob: heuristic_logprob(tokens),
                    text,
                    tokens,
                    finish,
                };
                if !casc.should_escalate(&outcome) {
                    info!(step = %step, tokens, "cascade streaming: early step accepted");
                    let _ = tx
                        .send(chunk_json(
                            &id,
                            created,
                            &model,
                            None,
                            Some(&outcome.text),
                            None,
                        ))
                        .await;
                    let _ = tx
                        .send(chunk_json(
                            &id,
                            created,
                            &model,
                            None,
                            None,
                            Some(&outcome.finish),
                        ))
                        .await;
                    return Ok(());
                }
                tracing::debug!(step = %step, logprob = outcome.logprob, "cascade escalating");
            }
            Err(e) => {
                warn!(step = %step, error=?e, "cascade step failed; escalating");
            }
        }
    }
    unreachable!("loop returns on the final step");
}

/// Streaming path: forward token deltas as `data: {...chunk...}\n\n`.
async fn stream_run(
    state: Arc<SharedState>,
    proto: GenerateRequest,
    tx: mpsc::Sender<String>,
    id: String,
    model: String,
    created: i64,
) -> cgn_core::Result<()> {
    // First chunk announces the role.
    let _ = tx
        .send(chunk_json(
            &id,
            created,
            &model,
            Some("assistant"),
            None,
            None,
        ))
        .await;
    stream_tokens(state, proto, tx, id, model, created).await
}

/// Pump engine tokens into SSE chunks. Assumes the role chunk has
/// already been sent.
async fn stream_tokens(
    state: Arc<SharedState>,
    proto: GenerateRequest,
    tx: mpsc::Sender<String>,
    id: String,
    model: String,
    created: i64,
) -> cgn_core::Result<()> {
    let mut stream = run_to_token_stream(state, proto).await?;
    while let Some(item) = stream.next().await {
        match item {
            Ok(t) => {
                let chunk = ChatChunk {
                    id: id.clone(),
                    object: "chat.completion.chunk",
                    created,
                    model: model.clone(),
                    choices: vec![ChatChunkChoice {
                        index: 0,
                        delta: ChatDelta {
                            role: None,
                            content: if t.text.is_empty() {
                                None
                            } else {
                                Some(t.text.clone())
                            },
                        },
                        finish_reason: if t.finish.is_empty() {
                            None
                        } else {
                            Some(t.finish.clone())
                        },
                    }],
                };
                if tx
                    .send(serde_json::to_string(&chunk).unwrap())
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            Err(e) => {
                let chunk = ChatChunk {
                    id: id.clone(),
                    object: "chat.completion.chunk",
                    created,
                    model: model.clone(),
                    choices: vec![ChatChunkChoice {
                        index: 0,
                        delta: ChatDelta::default(),
                        finish_reason: Some(format!("error:{}", e.code())),
                    }],
                };
                let _ = tx.send(serde_json::to_string(&chunk).unwrap()).await;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Non-streaming path: collect all tokens and return them as a single body.
async fn buffered_run(
    state: Arc<SharedState>,
    proto: GenerateRequest,
) -> cgn_core::Result<(String, u32, String)> {
    let mut stream = run_to_token_stream(state, proto).await?;
    let mut text = String::new();
    let mut count = 0u32;
    let mut finish = "stop".to_string();
    while let Some(item) = stream.next().await {
        match item {
            Ok(t) => {
                text.push_str(&t.text);
                count += 1;
                if !t.finish.is_empty() {
                    finish = t.finish;
                }
            }
            Err(e) => {
                return Err(cgn_core::Error::Internal(format!("stream: {e}")));
            }
        }
    }
    info!(tokens = count, "chat completion finished");
    Ok((text, count, finish))
}

/// Maximum dispatch attempts per request. The first failure excludes
/// the failed node and re-picks; a second failure gives up (small
/// clusters run out of alternatives quickly and the client's own retry
/// budget is usually tighter than ours).
const MAX_DISPATCH_ATTEMPTS: usize = 3;

/// Drive the routing pipeline and return a stream of tokens. Currently the
/// router talks to the agent over gRPC; this helper hides that detail so
/// the gateway handlers are pure HTTP code.
///
/// Dispatch failures (agent unreachable, gRPC connect/setup error) are
/// retried against the next-best node with the failed node excluded, up
/// to [`MAX_DISPATCH_ATTEMPTS`]. Retries happen strictly *before* any
/// token has been produced, so they are invisible to the client (for
/// streaming responses the SSE role chunk is emitted independently).
/// Mid-stream errors remain terminal — resuming half-generated output
/// safely requires token-state migration, which is future work.
async fn run_to_token_stream(
    state: Arc<SharedState>,
    req: GenerateRequest,
) -> cgn_core::Result<
    impl futures::Stream<Item = Result<cgn_proto::v1::Token, tonic::Status>> + Unpin,
> {
    let token_ids = approximate_token_ids(&join_messages(&req.messages));
    let mut exclude: Vec<String> = Vec::new();

    for attempt in 1..=MAX_DISPATCH_ATTEMPTS {
        match dispatch_once(&state, &req, &token_ids, &exclude).await {
            Ok(stream) => return Ok(stream),
            Err((failed_node, e)) => {
                let retryable = matches!(
                    e,
                    cgn_core::Error::Unavailable(_) | cgn_core::Error::Internal(_)
                ) && failed_node.is_some();
                if !retryable || attempt == MAX_DISPATCH_ATTEMPTS {
                    return Err(e);
                }
                let node = failed_node.expect("checked above");
                warn!(
                    %node,
                    attempt,
                    error = ?e,
                    "dispatch failed; retrying on next-best node"
                );
                exclude.push(node);
            }
        }
    }
    unreachable!("loop returns or errors within MAX_DISPATCH_ATTEMPTS");
}

/// One dispatch attempt: plan → pick → (prefill) → decode. On failure
/// returns the decode node id (when one was chosen) so the caller can
/// exclude it and retry.
async fn dispatch_once(
    state: &Arc<SharedState>,
    req: &GenerateRequest,
    token_ids: &[u32],
    exclude: &[String],
) -> Result<
    impl futures::Stream<Item = Result<cgn_proto::v1::Token, tonic::Status>> + Unpin,
    (Option<String>, cgn_core::Error),
> {
    use crate::disagg::{self, Plan};
    use cgn_proto::v1::AgentGenerateRequest;

    let prompt_tokens = token_ids.len() as u32;

    // Disagg plan: maybe split into (prefill, decode) or stay colocate.
    let cfg_disagg = &state.cfg.router.disagg;
    let plan = disagg::plan(
        cfg_disagg.enabled,
        cfg_disagg.colocate_below_tokens,
        prompt_tokens,
    );

    let (prefill_decision, decode_decision) = match plan {
        Plan::Colocate => {
            let d = routing::pick_excluding(state, &req.model, NodeRole::Both, token_ids, exclude)
                .await
                .map_err(|e| (None, e))?;
            (d.clone(), d)
        }
        Plan::Split {
            prefill_role,
            decode_role,
        } => {
            let (p, d) = routing::pick_pair(
                state,
                &req.model,
                prefill_role,
                decode_role,
                token_ids,
                exclude,
            )
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
    let mut client = state
        .connect_agent(&decode_decision.node.address)
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
    };
    let req_stream = futures::stream::iter(vec![agent_req]);
    let response = client
        .generate(tonic::Request::new(req_stream))
        .await
        .map_err(|s| {
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

    Ok(response)
}

/// Issue a prefill-only request to the prefill agent and collect the
/// returned block list. On any error returns an empty vec and the
/// caller falls back to colocate execution.
async fn run_prefill(
    state: Arc<SharedState>,
    address: &str,
    req: &cgn_proto::v1::GenerateRequest,
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

fn join_messages(msgs: &[PMessage]) -> String {
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

fn approximate_token_ids(s: &str) -> Vec<u32> {
    s.split_whitespace()
        .map(|w| {
            let h = blake3::hash(w.as_bytes());
            let b = h.as_bytes();
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        })
        .collect()
}

fn error_json(e: &cgn_core::Error) -> Response {
    let (status, code) = match e {
        cgn_core::Error::InvalidArgument(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
        cgn_core::Error::NotFound(_) => (StatusCode::NOT_FOUND, "not_found_error"),
        cgn_core::Error::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "server_error"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "server_error"),
    };
    (
        status,
        Json(json!({
            "error": {
                "message": e.to_string(),
                "type": code,
                "code": null,
            }
        })),
    )
        .into_response()
}
