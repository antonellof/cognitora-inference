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
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use cgn_proto::v1::{GenerateRequest, Message as PMessage, NodeRole, SamplingParams};
use futures::StreamExt;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};

use crate::carbon::{self, CarbonAdmission, RequestPriority};
use crate::cascade::{Cascade, StepOutcome};
use crate::routing;
use crate::state::SharedState;

use super::metrics::{CHAT_COMPLETION_TOKENS, CHAT_LATENCY, CHAT_REQUESTS, CHAT_TTFT};
use super::sse;
use super::types::{
    ChatChoice, ChatChunk, ChatChunkChoice, ChatDelta, ChatMessage, ChatRequest, ChatResponse,
    Usage,
};

pub async fn completions(
    State(state): State<Arc<SharedState>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Response {
    let priority = headers
        .get("x-cgn-priority")
        .and_then(|v| v.to_str().ok())
        .map(RequestPriority::parse_header)
        .unwrap_or(RequestPriority::Normal);
    match carbon::check_admission(
        &state.cfg.carbon,
        state.carbon.snapshot().as_ref(),
        priority,
    ) {
        CarbonAdmission::Admit => {}
        CarbonAdmission::Reject {
            intensity,
            threshold,
        } => {
            carbon::record_rejection();
            CHAT_REQUESTS
                .with_label_values(&[&req.model, "429"])
                .inc();
            return carbon_reject_response(intensity, threshold);
        }
    }

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
        // Tool-calling / structured-output requests bypass the cascade:
        // different cascade models emit incompatible tool-call formats,
        // and guided decoding must run on the model the client asked for.
        let casc = if proto_req.extensions_json.is_empty() {
            Cascade::from_config(&state.cfg, &model, &[])
        } else {
            None
        };
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
    // confidence before deciding whether to escalate. Tool/structured
    // requests skip the cascade (see the streaming path for why).
    let casc = if proto_req.extensions_json.is_empty() {
        Cascade::from_config(&state.cfg, &model, &[])
    } else {
        None
    };
    let result = match casc {
        Some(c) => buffered_with_cascade(state.clone(), proto_req, c).await,
        None => buffered_run(state.clone(), proto_req)
            .await
            .map(|(text, n, finish, tool_calls)| (text, n, finish, tool_calls, model.clone())),
    };

    let dt = started.elapsed().as_secs_f64();
    CHAT_LATENCY
        .with_label_values(&[&model, stream_label])
        .observe(dt);

    match result {
        Ok((text, completion_tokens, finish, tool_calls, used_model)) => {
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
                        content: super::types::ContentSpec::Text(text),
                        name: None,
                        tool_calls,
                        tool_call_id: None,
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
) -> cgn_core::Result<(String, u32, String, Option<serde_json::Value>, String)> {
    let result = casc
        .run(|model| {
            let state = state.clone();
            let mut step_proto = proto.clone();
            step_proto.model = model.to_string();
            async move {
                match buffered_run(state, step_proto).await {
                    Ok((text, n, finish, _tool_calls)) => StepOutcome {
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
        // Cascade requests never carry tools (the gateway bypasses the
        // cascade when extensions are present), so no tool calls here.
        None,
        result.model_used,
    ))
}

fn build_proto_request(r: &ChatRequest) -> GenerateRequest {
    use super::types::ContentSpec;
    let messages: Vec<PMessage> = r
        .messages
        .iter()
        .map(|m| {
            let (content, content_json) = match &m.content {
                ContentSpec::Text(s) => (s.clone(), String::new()),
                // Multimodal content parts: keep the raw JSON for the
                // engine and a text-only view for prefix hashing.
                ContentSpec::Parts(v) => (String::new(), v.to_string()),
            };
            PMessage {
                role: m.role.clone(),
                content,
                name: m.name.clone().unwrap_or_default(),
                content_json,
                tool_calls_json: m
                    .tool_calls
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                tool_call_id: m.tool_call_id.clone().unwrap_or_default(),
            }
        })
        .collect();

    // Tool / structured-output passthrough. Bundled as one JSON object
    // so the wire protocol stays stable as OpenAI grows the surface.
    let mut ext = serde_json::Map::new();
    if let Some(t) = &r.tools {
        ext.insert("tools".into(), t.clone());
    }
    if let Some(t) = &r.tool_choice {
        ext.insert("tool_choice".into(), t.clone());
    }
    if let Some(t) = &r.response_format {
        ext.insert("response_format".into(), t.clone());
    }
    let extensions_json = if ext.is_empty() {
        String::new()
    } else {
        serde_json::Value::Object(ext).to_string()
    };

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
        extensions_json,
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
                tool_calls: None,
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
            Ok((text, tokens, finish, _tool_calls)) => {
                let outcome = StepOutcome {
                    logprob: heuristic_logprob(tokens),
                    text,
                    tokens,
                    finish,
                };
                if !casc.should_escalate(&outcome) {
                    info!(step = %step, tokens, "cascade streaming: early step accepted");
                    CHAT_COMPLETION_TOKENS
                        .with_label_values(&[&model])
                        .inc_by(outcome.tokens as u64);
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
    let dispatch_started = std::time::Instant::now();
    let mut stream = run_to_token_stream(state, proto).await?;
    let mut completion_tokens = 0u64;
    let mut first_token_seen = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(t) => {
                if !first_token_seen {
                    first_token_seen = true;
                    CHAT_TTFT
                        .with_label_values(&[&model])
                        .observe(dispatch_started.elapsed().as_secs_f64());
                }
                if !t.text.is_empty() || !t.tool_calls_json.is_empty() {
                    completion_tokens += 1;
                }
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
                            // Tool-call deltas pass through verbatim.
                            tool_calls: if t.tool_calls_json.is_empty() {
                                None
                            } else {
                                serde_json::from_str(&t.tool_calls_json).ok()
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
                    break; // client disconnected
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
                break;
            }
        }
    }
    // Streaming responses previously never fed the completion-tokens
    // counter (only the buffered path did), silently under-reporting
    // token throughput for streaming-heavy workloads.
    if completion_tokens > 0 {
        CHAT_COMPLETION_TOKENS
            .with_label_values(&[&model])
            .inc_by(completion_tokens);
    }
    Ok(())
}

/// Non-streaming path: collect all tokens and return them as a single
/// body. Tool-call deltas are aggregated OpenAI-style: entries with the
/// same `index` are merged, with `function.arguments` fragments
/// concatenated in arrival order.
async fn buffered_run(
    state: Arc<SharedState>,
    proto: GenerateRequest,
) -> cgn_core::Result<(String, u32, String, Option<serde_json::Value>)> {
    let mut stream = run_to_token_stream(state, proto).await?;
    let mut text = String::new();
    let mut count = 0u32;
    let mut finish = "stop".to_string();
    let mut tool_agg = ToolCallAggregator::default();
    while let Some(item) = stream.next().await {
        match item {
            Ok(t) => {
                text.push_str(&t.text);
                // Don't count the synthetic empty terminator chunk (or
                // finish-only frames) as a completion token.
                if !t.text.is_empty() || !t.tool_calls_json.is_empty() {
                    count += 1;
                }
                if !t.tool_calls_json.is_empty() {
                    if let Ok(v) = serde_json::from_str(&t.tool_calls_json) {
                        tool_agg.push(&v);
                    }
                }
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
    Ok((text, count, finish, tool_agg.finish()))
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
///
/// When the *local* cluster has no eligible node at all (routing itself
/// failed, not a dispatch to a chosen node) and `[router.federation]` is
/// enabled, the request is forwarded to the lowest-latency reachable
/// peer cluster instead of failing. Forwarding targets the peer's gRPC
/// surface, whose handler only routes locally — so a request crosses at
/// most one cluster boundary and cannot loop.
async fn run_to_token_stream(
    state: Arc<SharedState>,
    req: GenerateRequest,
) -> cgn_core::Result<
    futures::stream::BoxStream<'static, Result<cgn_proto::v1::Token, tonic::Status>>,
> {
    let token_ids =
        routing::prompt::approximate_token_ids(&routing::prompt::join_messages(&req.messages));
    let mut exclude: Vec<String> = Vec::new();

    for attempt in 1..=MAX_DISPATCH_ATTEMPTS {
        match dispatch_once(&state, &req, &token_ids, &exclude).await {
            Ok(stream) => return Ok(Box::pin(stream)),
            Err((failed_node, e)) => {
                // Routing found no local candidate: try peer clusters.
                if failed_node.is_none() && matches!(e, cgn_core::Error::Unavailable(_)) {
                    let fed = &state.cfg.router.federation;
                    if fed.enabled && !fed.peers.is_empty() {
                        match federate(&state, &req).await {
                            Ok(stream) => return Ok(stream),
                            Err(fe) => {
                                warn!(error = ?fe, "federation fallback failed");
                            }
                        }
                    }
                    return Err(e);
                }
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

/// Forward the request to the best federation peer and return its token
/// stream. See [`crate::federation`].
async fn federate(
    state: &Arc<SharedState>,
    req: &GenerateRequest,
) -> cgn_core::Result<
    futures::stream::BoxStream<'static, Result<cgn_proto::v1::Token, tonic::Status>>,
> {
    let fed = &state.cfg.router.federation;
    let (peer, mut client) = crate::federation::pick_peer(&fed.peers, &req.model).await?;
    let stream = crate::federation::forward(&mut client, req.clone()).await?;
    info!(%peer, model = %req.model, "no local node; request federated to peer cluster");
    super::metrics::FEDERATION_FORWARDS
        .with_label_values(&[&req.model, &peer])
        .inc();
    Ok(Box::pin(stream))
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
        extensions_json: req.extensions_json.clone(),
        // The agent publishes these as *confirmed* KV claims to etcd
        // after the generation completes successfully.
        digests: decode_decision.digests.iter().map(|d| d.to_vec()).collect(),
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
        extensions_json: req.extensions_json.clone(),
        digests: vec![],
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

fn carbon_reject_response(intensity: f64, threshold: f64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": {
                "message": format!(
                    "grid carbon intensity {:.0} gCO2/kWh exceeds threshold {:.0}; \
                     low-priority requests deferred until intensity drops",
                    intensity, threshold
                ),
                "type": "server_error",
                "code": "carbon_intensity",
            }
        })),
    )
        .into_response()
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
