//! `/v1/chat/completions` and legacy `/v1/completions`.
//!
//! Translates OpenAI request shapes into a `cognitora.v1` proto
//! `GenerateRequest`, hands it to [`super::generate`], and renders the
//! result as one JSON body or Server-Sent Events.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use cgn_auth::Principal;
use cgn_proto::v1::{GenerateRequest, Message as PMessage, SamplingParams};
use serde_json::json;
use tracing::warn;

use crate::carbon::{self, CarbonAdmission, RequestPriority};
use crate::cascade::Cascade;
use crate::state::SharedState;

use super::authz::{authorize, Caller};
use super::generate::{self, error_json, Generated, Resolved, ResponseCtx, Surface};
use super::metrics::{CHAT_LATENCY, CHAT_REQUESTS};
use super::types::{
    ChatChoice, ChatMessage, ChatRequest, ChatResponse, CompletionRequest, ContentSpec, PromptSpec,
    TextChoice, TextCompletion,
};

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": {
                "message": message.into(),
                "type": "invalid_request_error",
                "code": null,
            }
        })),
    )
        .into_response()
}

/// Requests that must run on the model the client named: tool formats
/// differ between cascade models, and guided decoding binds to one model.
fn pins_model(r: &ChatRequest) -> bool {
    r.tools.is_some() || r.tool_choice.is_some() || r.response_format.is_some()
}

/// Shared by both surfaces once the proto is built.
async fn serve(
    state: Arc<SharedState>,
    headers: &HeaderMap,
    mut proto: GenerateRequest,
    casc: Option<Cascade>,
    ctx: ResponseCtx,
    stream: bool,
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
            CHAT_REQUESTS.with_label_values(&[&ctx.model, "429"]).inc();
            return carbon_reject_response(intensity, threshold);
        }
    }

    generate::apply_headers(&mut proto, headers);
    let started = Instant::now();
    let resolved = match &casc {
        Some(c) => generate::resolve_cascade(&state, &proto, c).await,
        None => generate::open(&state, &proto).await.map(Resolved::Live),
    };
    let resolved = match resolved {
        Ok(r) => r,
        Err(e) => {
            // Nothing has been sent, so streaming clients get a real
            // status too, not a 200 with an empty stream.
            warn!(error = ?e, "completion failed");
            return count_error(&ctx.model, error_json(&e));
        }
    };

    if stream {
        return generate::stream_response(state, resolved, ctx, started);
    }

    let generated = match resolved {
        Resolved::Done(g) => Ok(g),
        Resolved::Live(flow) => generate::collect(flow).await,
    };
    CHAT_LATENCY
        .with_label_values(&[&ctx.model, "false"])
        .observe(started.elapsed().as_secs_f64());
    match generated {
        Ok(g) => {
            CHAT_REQUESTS.with_label_values(&[&ctx.model, "200"]).inc();
            generate::settle(&state, &ctx, g.usage);
            buffered_response(g, &ctx)
        }
        Err(e) => {
            warn!(error = ?e, "completion failed");
            count_error(&ctx.model, error_json(&e))
        }
    }
}

/// Count a failed request. Server faults share one `5xx` label so alerts
/// can use it for the error rate; client errors and overload keep their
/// own code, because neither means the service is broken.
fn count_error(model: &str, resp: Response) -> Response {
    let status = resp.status();
    let label = if status.is_server_error() {
        "5xx"
    } else {
        status.as_str()
    };
    CHAT_REQUESTS.with_label_values(&[model, label]).inc();
    resp
}

fn buffered_response(g: Generated, ctx: &ResponseCtx) -> Response {
    match ctx.surface {
        Surface::Chat => Json(ChatResponse {
            id: ctx.id.clone(),
            object: "chat.completion",
            created: ctx.created,
            model: ctx.model.clone(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content: ContentSpec::Text(g.text),
                    name: None,
                    tool_calls: g.tool_calls,
                    tool_call_id: None,
                },
                finish_reason: g.finish,
                logprobs: g.logprobs,
            }],
            usage: g.usage,
        })
        .into_response(),
        Surface::Text => Json(TextCompletion {
            id: ctx.id.clone(),
            object: "text_completion",
            created: ctx.created,
            model: ctx.model.clone(),
            choices: vec![TextChoice {
                index: 0,
                text: g.text,
                finish_reason: Some(g.finish),
                logprobs: None,
            }],
            usage: Some(g.usage),
        })
        .into_response(),
    }
}

fn response_ctx(
    prefix: &str,
    model: &str,
    surface: Surface,
    include_usage: bool,
    caller: Caller,
) -> ResponseCtx {
    ResponseCtx {
        id: format!(
            "{prefix}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        ),
        created: chrono::Utc::now().timestamp(),
        model: model.to_string(),
        surface,
        include_usage,
        caller,
    }
}

pub async fn completions(
    State(state): State<Arc<SharedState>>,
    principal: Option<Extension<Principal>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Response {
    let caller = match authorize(&state, principal.as_deref(), "chat", &req.model) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if req.n.is_some_and(|n| n != 1) {
        return bad_request("only n=1 is supported");
    }
    let stream = req.stream.unwrap_or(false);
    let casc = if pins_model(&req) {
        None
    } else {
        Cascade::from_config(&state.cfg, &req.model, &[])
    };
    let include_usage = req.stream_options.as_ref().is_some_and(|o| o.include_usage);
    let ctx = response_ctx(
        "chatcmpl",
        &req.model,
        Surface::Chat,
        include_usage,
        caller.clone(),
    );
    let proto = build_proto_request(&state, &req, &caller);
    serve(state, &headers, proto, casc, ctx, stream).await
}

/// Legacy text completions: a raw prompt with no chat template.
pub async fn text_completions(
    State(state): State<Arc<SharedState>>,
    principal: Option<Extension<Principal>>,
    headers: HeaderMap,
    Json(req): Json<CompletionRequest>,
) -> Response {
    let caller = match authorize(&state, principal.as_deref(), "chat", &req.model) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if req.n.is_some_and(|n| n != 1) {
        return bad_request("only n=1 is supported");
    }
    if req.echo == Some(true) {
        return bad_request("echo is not supported");
    }
    if req.logprobs.is_some() {
        return bad_request("logprobs is not supported on /v1/completions");
    }
    let prompt = match req.prompt {
        PromptSpec::One(p) => p,
        PromptSpec::Many(mut v) if v.len() == 1 => v.remove(0),
        PromptSpec::Many(_) => return bad_request("prompt arrays must hold exactly one string"),
    };
    if prompt.is_empty() {
        return bad_request("prompt must not be empty");
    }
    let stream = req.stream.unwrap_or(false);
    let include_usage = req.stream_options.as_ref().is_some_and(|o| o.include_usage);
    let ctx = response_ctx(
        "cmpl",
        &req.model,
        Surface::Text,
        include_usage,
        caller.clone(),
    );
    let proto = GenerateRequest {
        model: req.model.clone(),
        prompt,
        params: Some(SamplingParams {
            temperature: req.temperature.unwrap_or(1.0),
            top_p: req.top_p.unwrap_or(1.0),
            max_tokens: req.max_tokens.unwrap_or(0),
            stop: req.stop.clone().map(|s| s.into_vec()).unwrap_or_default(),
            seed: req.seed,
            frequency_penalty: req.frequency_penalty.unwrap_or(0.0),
            presence_penalty: req.presence_penalty.unwrap_or(0.0),
            repetition_penalty: 1.0,
            ..Default::default()
        }),
        tenant: caller.tenant.clone(),
        session: req.user.clone().unwrap_or_default(),
        stream,
        extensions_json: isolation_ext(&state, &caller, serde_json::Map::new()),
        ..Default::default()
    };
    serve(state, &headers, proto, None, ctx, stream).await
}

/// Serialize request extensions, adding vLLM's `cache_salt` for tenants so
/// isolation also holds inside the engine's own prefix cache.
fn isolation_ext(
    state: &SharedState,
    caller: &Caller,
    mut ext: serde_json::Map<String, serde_json::Value>,
) -> String {
    if !caller.tenant.is_empty() && state.cfg.router.kv_tenant_isolation {
        let salt = blake3::hash(caller.tenant.as_bytes());
        ext.insert("cache_salt".into(), salt.to_hex().as_str().into());
    }
    if ext.is_empty() {
        String::new()
    } else {
        serde_json::Value::Object(ext).to_string()
    }
}

/// Translate an OpenAI chat request into the internal proto shape.
fn build_proto_request(state: &SharedState, r: &ChatRequest, caller: &Caller) -> GenerateRequest {
    let messages: Vec<PMessage> = r
        .messages
        .iter()
        .map(|m| {
            let (content, content_json) = match &m.content {
                ContentSpec::Text(s) => (s.clone(), String::new()),
                // Multimodal content parts: keep the raw JSON for the
                // engine; `join_messages` derives the hashed view from it.
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

    // Fields with no proto slot travel as one JSON object, so the wire
    // protocol stays stable as OpenAI grows the surface.
    let mut ext = serde_json::Map::new();
    let mut put = |k: &str, v: Option<serde_json::Value>| {
        if let Some(v) = v {
            ext.insert(k.to_string(), v);
        }
    };
    put("tools", r.tools.clone());
    put("tool_choice", r.tool_choice.clone());
    put("response_format", r.response_format.clone());
    put("logit_bias", r.logit_bias.clone());
    put("parallel_tool_calls", r.parallel_tool_calls.map(Into::into));
    put("top_logprobs", r.top_logprobs.map(Into::into));
    put("min_p", r.min_p.map(Into::into));

    GenerateRequest {
        model: r.model.clone(),
        messages,
        params: Some(SamplingParams {
            temperature: r.temperature.unwrap_or(1.0),
            top_p: r.top_p.unwrap_or(1.0),
            top_k: r.top_k.unwrap_or(0),
            max_tokens: r.max_completion_tokens.or(r.max_tokens).unwrap_or(0),
            stop: r.stop.clone().map(|s| s.into_vec()).unwrap_or_default(),
            logprobs: r.logprobs.unwrap_or(false),
            seed: r.seed,
            frequency_penalty: r.frequency_penalty.unwrap_or(0.0),
            presence_penalty: r.presence_penalty.unwrap_or(0.0),
            repetition_penalty: r.repetition_penalty.unwrap_or(1.0),
        }),
        tenant: caller.tenant.clone(),
        session: r.user.clone().unwrap_or_default(),
        stream: r.stream.unwrap_or(false),
        extensions_json: isolation_ext(state, caller, ext),
        ..Default::default()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use cgn_core::config::Config;

    fn parse(body: serde_json::Value) -> ChatRequest {
        serde_json::from_value(body).unwrap()
    }

    fn proto(body: serde_json::Value) -> GenerateRequest {
        let s = SharedState::build(Config::default());
        build_proto_request(&s, &parse(body), &Caller::default())
    }

    #[test]
    fn max_completion_tokens_wins_over_max_tokens() {
        let p =
            proto(json!({"model":"m","messages":[],"max_tokens":5,"max_completion_tokens":900}));
        assert_eq!(p.params.unwrap().max_tokens, 900);
        let p = proto(json!({"model":"m","messages":[],"max_tokens":5}));
        assert_eq!(p.params.unwrap().max_tokens, 5);
    }

    #[test]
    fn unset_max_tokens_stays_unset() {
        let p = proto(json!({"model":"m","messages":[]}));
        assert_eq!(p.params.unwrap().max_tokens, 0);
    }

    #[test]
    fn sampling_fields_are_carried() {
        let p = proto(json!({
            "model":"m","messages":[],"seed":0,"frequency_penalty":0.5,
            "presence_penalty":-0.5,"logprobs":true,"top_k":40
        }));
        let s = p.params.unwrap();
        assert_eq!(s.seed, Some(0));
        assert_eq!((s.frequency_penalty, s.presence_penalty), (0.5, -0.5));
        assert!(s.logprobs);
        assert_eq!(s.top_k, 40);
    }

    #[test]
    fn extensions_carry_fields_without_a_proto_slot() {
        let p = proto(json!({
            "model":"m","messages":[],"logit_bias":{"1":2},
            "parallel_tool_calls":false,"top_logprobs":3,"min_p":0.1
        }));
        let e: serde_json::Value = serde_json::from_str(&p.extensions_json).unwrap();
        assert_eq!(e["logit_bias"]["1"], 2);
        assert_eq!(e["parallel_tool_calls"], false);
        assert_eq!(e["top_logprobs"], 3);
    }

    #[test]
    fn client_user_field_is_a_session_not_a_tenant() {
        let p = proto(json!({"model":"m","messages":[],"user":"victim-tenant"}));
        assert_eq!(p.tenant, "");
        assert_eq!(p.session, "victim-tenant");
    }

    #[test]
    fn tenants_get_an_engine_cache_salt() {
        let s = SharedState::build(Config::default());
        let caller = Caller {
            subject: "k".into(),
            tenant: "acme".into(),
        };
        let r = parse(json!({"model":"m","messages":[]}));
        let p = build_proto_request(&s, &r, &caller);
        assert_eq!(p.tenant, "acme");
        assert!(p.extensions_json.contains("cache_salt"));
        assert!(
            !p.extensions_json.contains("acme"),
            "salt must not leak the tenant"
        );
    }

    #[test]
    fn cache_salt_alone_does_not_bypass_the_cascade() {
        let r = parse(json!({"model":"m","messages":[]}));
        assert!(!pins_model(&r));
        let tools = parse(json!({"model":"m","messages":[],"tools":[]}));
        assert!(pins_model(&tools));
    }

    #[test]
    fn legacy_prompt_parses_as_a_string_or_one_item_array() {
        let r: CompletionRequest =
            serde_json::from_value(json!({"model":"m","prompt":"hi"})).unwrap();
        assert!(matches!(r.prompt, PromptSpec::One(_)));
        let r: CompletionRequest =
            serde_json::from_value(json!({"model":"m","prompt":["hi"]})).unwrap();
        assert!(matches!(r.prompt, PromptSpec::Many(_)));
        assert!(
            serde_json::from_value::<CompletionRequest>(json!({"model":"m","prompt":[1,2]}))
                .is_err()
        );
    }
}
