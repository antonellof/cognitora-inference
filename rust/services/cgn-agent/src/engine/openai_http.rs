//! Generic driver for any OpenAI-compatible HTTP engine.
//!
//! vLLM, llama.cpp's `python -m llama_cpp.server`, the standalone
//! `llama-server` binary, sgLang, TGI, and several proxy services all
//! expose a `/v1/completions` route that streams Server-Sent Events with
//! the same shape:
//!
//! ```text
//! data: {"choices":[{"text":"...","finish_reason":null}]}
//!
//! data: [DONE]
//! ```
//!
//! Cognitora doesn't care which engine is on the other end — this driver
//! hits whichever HTTP server is running at `engine.url`.

use std::time::Duration;

use async_trait::async_trait;
use cgn_core::{Error, Result};
use cgn_proto::v1::Token;
use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::warn;

use super::{EmbedReq, EmbedResp, Engine, GenerateReq, ModelSpec};

/// HTTP driver for any OpenAI-compatible inference engine.
pub struct OpenAiHttpEngine {
    client: reqwest::Client,
    base: String,
    /// Logged in tracing spans and used for metric labels. e.g. "vllm",
    /// "llama_cpp", "openai_compat".
    kind: &'static str,
}

impl OpenAiHttpEngine {
    pub fn new(kind: &'static str, base_url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            // Only stalls are fatal: a long generation may stream for hours.
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(600))
            .pool_max_idle_per_host(8)
            .build()
            .map_err(|e| Error::Internal(format!("reqwest: {e}")))?;
        Ok(Self {
            client,
            base: base_url.into().trim_end_matches('/').to_string(),
            kind,
        })
    }
}

#[async_trait]
impl Engine for OpenAiHttpEngine {
    fn name(&self) -> &'static str {
        self.kind
    }

    async fn load_model(&self, _spec: ModelSpec) -> Result<()> {
        // Engines load their model when spawned; the supervisor handles
        // process lifecycle. This call exists to support engines that
        // accept dynamic model swaps over their control plane.
        Ok(())
    }

    async fn generate(&self, req: GenerateReq, tx: mpsc::Sender<Token>) -> Result<()> {
        // Prefer the chat completions endpoint whenever we have
        // structured messages — it makes the engine apply the model's
        // chat template, which is the difference between coherent
        // output and gibberish for instruct/chat-tuned models. Fall
        // back to legacy `/v1/completions` only when the caller really
        // did pass a raw prompt.
        let chat_mode = !req.messages.is_empty();
        let url = if chat_mode {
            format!("{}/v1/chat/completions", self.base)
        } else {
            format!("{}/v1/completions", self.base)
        };

        let mut body = if chat_mode {
            let messages: Vec<_> = req.messages.iter().map(message_json).collect();
            serde_json::json!({ "model": req.model, "messages": messages })
        } else {
            serde_json::json!({ "model": req.model, "prompt": req.prompt })
        };
        apply_sampling(&mut body, &req, chat_mode);

        // Merge OpenAI extensions (tools / tool_choice / response_format)
        // into the body verbatim — the engine implements them (vLLM and
        // SGLang both support tool parsing and guided decoding).
        if !req.extensions_json.is_empty() {
            match serde_json::from_str::<serde_json::Value>(&req.extensions_json) {
                Ok(serde_json::Value::Object(ext)) => {
                    if let Some(obj) = body.as_object_mut() {
                        for (k, v) in ext {
                            // `cache_salt` is a vLLM extension; other
                            // engines may reject the unknown field.
                            if k == "cache_salt" && self.kind != "vllm" {
                                continue;
                            }
                            obj.insert(k, v);
                        }
                    }
                }
                _ => warn!(engine = self.kind, "ignoring unparsable extensions_json"),
            }
        }

        if let Some(kv) =
            kv_transfer_params_from_digests(&req.prefix_digests, &req.resident_digests)
        {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("kv_transfer_params".into(), kv);
            }
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("{} post: {e}", self.kind)))?;

        if !resp.status().is_success() {
            let s = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            return Err(classify_engine_status(self.kind, s, &txt));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = Vec::with_capacity(8192);
        let id = req.id.clone();
        let forward_logprobs = req.logprobs;
        let mut saw_finish = false;
        let mut usage = (0u32, 0u32);

        while let Some(item) = stream.next().await {
            let bytes = item.map_err(|e| Error::Internal(format!("{} stream: {e}", self.kind)))?;
            buf.extend_from_slice(&bytes);
            // SSE frames are `data: <json>` terminated by a blank line.
            // Servers may use LF (`\n\n`) or CRLF (`\r\n\r\n`) framing —
            // treating only LF as a delimiter would never complete a
            // frame on a CRLF stream and grow `buf` without bound.
            while let Some(end) = find_frame_end(&buf) {
                let frame = buf.drain(..end).collect::<Vec<u8>>();
                let text = std::str::from_utf8(&frame).unwrap_or("");
                // A frame may carry `event:`/`id:` lines alongside the
                // `data:` line; pick the data line rather than requiring
                // the whole frame to start with it.
                let Some(line) = text.lines().map(str::trim).find(|l| l.starts_with("data:"))
                else {
                    continue;
                };
                let payload = line.trim_start_matches("data:").trim();
                if payload == "[DONE]" {
                    let _ = tx.send(terminal_token(&id, saw_finish, usage)).await;
                    return Ok(());
                }
                match serde_json::from_str::<StreamFrame>(payload) {
                    Ok(f) => {
                        // `stream_options.include_usage` engines report
                        // usage on a final frame with empty `choices`.
                        if let Some(u) = f.usage {
                            usage = (u.prompt_tokens, u.completion_tokens);
                        }
                        for choice in f.choices {
                            // Chat-completions SSE puts the new text in
                            // `delta.content`; legacy completions put it
                            // in `text`. We accept whichever is present.
                            let text = choice
                                .delta
                                .as_ref()
                                .and_then(|d| d.content.clone())
                                .or(choice.text)
                                .unwrap_or_default();
                            // Tool-call deltas pass through as raw JSON.
                            let tool_calls_json = choice
                                .delta
                                .as_ref()
                                .and_then(|d| d.tool_calls.as_ref())
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            // Skip empty deltas (chat streams emit a
                            // role-only frame as the first chunk).
                            let finish = choice.finish_reason.unwrap_or_default();
                            if text.is_empty() && finish.is_empty() && tool_calls_json.is_empty() {
                                continue;
                            }
                            saw_finish |= !finish.is_empty();
                            let token = Token {
                                id: id.clone(),
                                text,
                                logprob: choice
                                    .logprobs
                                    .as_ref()
                                    .and_then(mean_logprob)
                                    .unwrap_or(0.0),
                                logprobs_json: choice
                                    .logprobs
                                    .as_ref()
                                    .filter(|_| forward_logprobs)
                                    .map(|v| v.to_string())
                                    .unwrap_or_default(),
                                finish,
                                tool_calls_json,
                                ..Token::default()
                            };
                            if tx.send(token).await.is_err() {
                                return Ok(()); // client gone
                            }
                        }
                    }
                    Err(e) => warn!(
                        error=?e,
                        engine=self.kind,
                        payload,
                        "skipping unparsable engine frame"
                    ),
                }
            }
        }
        // Some servers close without `[DONE]` after a finish reason, which
        // is fine. No finish reason at all means the answer was cut short.
        if !saw_finish {
            return Err(Error::Internal(format!(
                "{} stream ended before a finish reason",
                self.kind
            )));
        }
        let _ = tx.send(terminal_token(&id, saw_finish, usage)).await;
        Ok(())
    }

    async fn embed(&self, req: EmbedReq) -> Result<EmbedResp> {
        let url = format!("{}/v1/embeddings", self.base);
        let body = serde_json::json!({
            "model": req.model,
            "input": req.inputs,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("{} embed post: {e}", self.kind)))?;

        let status = resp.status();
        if !status.is_success() {
            let txt = resp.text().await.unwrap_or_default();
            // 404 from a server without /v1/embeddings is the most common
            // failure mode (e.g. vLLM serving a chat-only model). Surface
            // it as Unavailable so the router translates to 503 instead
            // of 500.
            let err = if status == reqwest::StatusCode::NOT_FOUND {
                Error::Unavailable(format!(
                    "{} returned 404 from /v1/embeddings — is the loaded model an embedding model?",
                    self.kind
                ))
            } else {
                Error::Internal(format!("{} embed status {status}: {txt}", self.kind))
            };
            return Err(err);
        }

        let parsed: EmbedFrame = resp
            .json()
            .await
            .map_err(|e| Error::Internal(format!("{} embed decode: {e}", self.kind)))?;

        let embeddings = parsed.data.into_iter().map(|d| d.embedding).collect();
        let prompt_tokens = parsed.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0);
        Ok(EmbedResp {
            embeddings,
            prompt_tokens,
        })
    }

    async fn ready(&self) -> bool {
        // Try the standard OpenAI-style endpoints. vLLM and llama.cpp both
        // expose /health; if it's missing we fall back to /v1/models which
        // every OpenAI-compatible server implements.
        for path in ["/health", "/v1/models"] {
            let url = format!("{}{}", self.base, path);
            if let Ok(r) = self
                .client
                .get(&url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                if r.status().is_success() {
                    return true;
                }
            }
        }
        false
    }
}

/// Translate `req` into OpenAI sampling fields, omitting anything unset so
/// each engine keeps its own defaults.
fn apply_sampling(body: &mut serde_json::Value, req: &GenerateReq, chat_mode: bool) {
    use serde_json::json;
    body["temperature"] = json!(req.temperature);
    body["top_p"] = json!(req.top_p);
    body["stop"] = json!(req.stop);
    body["stream"] = json!(true);
    body["stream_options"] = json!({ "include_usage": true });
    if req.max_tokens > 0 {
        body["max_tokens"] = json!(req.max_tokens);
    }
    if let Some(seed) = req.seed {
        body["seed"] = json!(seed);
    }
    if req.frequency_penalty != 0.0 {
        body["frequency_penalty"] = json!(req.frequency_penalty);
    }
    if req.presence_penalty != 0.0 {
        body["presence_penalty"] = json!(req.presence_penalty);
    }
    if req.repetition_penalty != 0.0 && req.repetition_penalty != 1.0 {
        body["repetition_penalty"] = json!(req.repetition_penalty);
    }
    if req.top_k > 0 {
        body["top_k"] = json!(req.top_k);
    }
    if req.logprobs {
        // Chat takes a bool; legacy completions take a count.
        body["logprobs"] = if chat_mode { json!(true) } else { json!(1) };
    }
}

/// Map an engine HTTP failure onto the error that decides retry behaviour:
/// the caller's fault stays a 4xx and is not retried, overload and server
/// faults may succeed on another node.
fn classify_engine_status(kind: &str, status: reqwest::StatusCode, body: &str) -> Error {
    let msg = format!("{kind} status {status}: {body}");
    match status.as_u16() {
        429 | 503 => Error::Unavailable(msg),
        408 => Error::Internal(msg),
        400..=499 => Error::InvalidArgument(msg),
        _ => Error::Internal(msg),
    }
}

/// End-of-stream token carrying usage. It adds a finish reason only when
/// the engine never sent one, so clients don't see two.
fn terminal_token(id: &str, saw_finish: bool, usage: (u32, u32)) -> Token {
    Token {
        id: id.to_string(),
        finish: if saw_finish {
            String::new()
        } else {
            "stop".into()
        },
        prompt_tokens: usage.0,
        completion_tokens: usage.1,
        ..Token::default()
    }
}

/// Mean logprob of one frame. Chat uses `logprobs.content[].logprob`,
/// legacy completions use `logprobs.token_logprobs[]`.
fn mean_logprob(v: &serde_json::Value) -> Option<f32> {
    let values: Vec<f64> = match v.get("content").and_then(|c| c.as_array()) {
        Some(items) => items
            .iter()
            .filter_map(|i| i.get("logprob")?.as_f64())
            .collect(),
        None => v
            .get("token_logprobs")?
            .as_array()?
            .iter()
            .filter_map(|x| x.as_f64())
            .collect(),
    };
    (!values.is_empty()).then(|| (values.iter().sum::<f64>() / values.len() as f64) as f32)
}

/// Find the end (exclusive) of the first complete SSE frame in `buf`,
/// i.e. one past its blank-line terminator. Handles both LF (`\n\n`)
/// and CRLF (`\r\n\r\n`, seen as `\n\r\n` after the previous line's
/// `\r`) framing.
fn find_frame_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' {
            if buf[i + 1] == b'\n' {
                return Some(i + 2);
            }
            if buf[i + 1] == b'\r' && buf.get(i + 2) == Some(&b'\n') {
                return Some(i + 3);
            }
        }
        i += 1;
    }
    None
}

/// Render one chat message as OpenAI JSON, restoring multimodal content
/// parts and tool-call fields carried through the proto verbatim.
fn message_json(m: &super::ChatMessage) -> serde_json::Value {
    let content: serde_json::Value = if !m.content_json.is_empty() {
        serde_json::from_str(&m.content_json)
            .unwrap_or_else(|_| serde_json::Value::String(m.content.clone()))
    } else {
        serde_json::Value::String(m.content.clone())
    };
    let mut msg = serde_json::json!({ "role": m.role, "content": content });
    if !m.tool_calls_json.is_empty() {
        if let Ok(tc) = serde_json::from_str::<serde_json::Value>(&m.tool_calls_json) {
            msg["tool_calls"] = tc;
        }
    }
    if !m.tool_call_id.is_empty() {
        msg["tool_call_id"] = m.tool_call_id.clone().into();
    }
    msg
}

#[derive(Deserialize)]
struct StreamFrame {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<FrameUsage>,
}

#[derive(Deserialize, Default)]
struct FrameUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

/// Holds whichever of the two SSE shapes the engine returns:
///
/// * Legacy `/v1/completions`: `{"text":"...","finish_reason":...}`
/// * Chat `/v1/chat/completions`: `{"delta":{"role":...,"content":"..."},
///   "finish_reason":...}`
///
/// We tolerate both and let the parsing site pick whichever is set.
#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    delta: Option<ChatDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
    /// Kept raw: chat and legacy completions use different shapes.
    #[serde(default)]
    logprobs: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct ChatDelta {
    #[serde(default)]
    content: Option<String>,
    /// Streaming tool-call fragments; kept as raw JSON and forwarded.
    #[serde(default)]
    tool_calls: Option<serde_json::Value>,
    // role is sent on the first chunk only; we don't currently use it.
    #[serde(default, rename = "role")]
    _role: Option<String>,
}

#[derive(Deserialize)]
struct EmbedFrame {
    data: Vec<EmbedDatum>,
    #[serde(default)]
    usage: Option<EmbedUsage>,
}

fn digest_hex(digests: &[Vec<u8>]) -> Vec<String> {
    digests
        .iter()
        .filter(|d| d.len() == 32)
        .map(|d| d.iter().map(|b| format!("{b:02x}")).collect())
        .collect()
}

/// Build vLLM `kv_transfer_params` from router-supplied prefix digests.
pub(crate) fn kv_transfer_params_from_digests(
    digests: &[Vec<u8>],
    resident: &[Vec<u8>],
) -> Option<serde_json::Value> {
    let prefix_hex = digest_hex(digests);
    if prefix_hex.is_empty() {
        return None;
    }
    let mut obj = serde_json::Map::new();
    obj.insert(
        "cgn_prefix_digests".into(),
        serde_json::Value::Array(
            prefix_hex
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    let resident_hex = digest_hex(resident);
    if !resident_hex.is_empty() {
        obj.insert(
            "cgn_resident_digests".into(),
            serde_json::Value::Array(
                resident_hex
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    Some(serde_json::Value::Object(obj))
}

#[derive(Deserialize)]
struct EmbedDatum {
    embedding: Vec<f32>,
}

#[derive(Deserialize, Default)]
struct EmbedUsage {
    #[serde(default)]
    prompt_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks down the chat-vs-legacy SSE shape handling that was at
    /// the root of [the empty-completion bug fixed in the same PR
    /// that introduced this test].
    #[test]
    fn parses_legacy_completions_text_field() {
        let payload = r#"{"choices":[{"text":"hello","finish_reason":null}]}"#;
        let f: StreamFrame = serde_json::from_str(payload).unwrap();
        let c = &f.choices[0];
        assert_eq!(c.text.as_deref(), Some("hello"));
        assert!(c.delta.is_none());
        assert_eq!(c.finish_reason.as_deref(), None);
    }

    #[test]
    fn parses_chat_completions_delta_content() {
        let payload = r#"{"choices":[{"delta":{"role":"assistant","content":" four"},"finish_reason":null}]}"#;
        let f: StreamFrame = serde_json::from_str(payload).unwrap();
        let c = &f.choices[0];
        assert!(c.text.is_none());
        let d = c.delta.as_ref().unwrap();
        assert_eq!(d.content.as_deref(), Some(" four"));
    }

    #[test]
    fn parses_chat_completions_role_only_first_chunk() {
        // The first chat-completions chunk carries `role` but no
        // `content`. We must accept and skip it without erroring.
        let payload = r#"{"choices":[{"delta":{"role":"assistant"}}]}"#;
        let f: StreamFrame = serde_json::from_str(payload).unwrap();
        let c = &f.choices[0];
        let d = c.delta.as_ref().unwrap();
        assert_eq!(d.content, None);
    }

    #[test]
    fn parses_tool_call_delta() {
        let payload = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#;
        let f: StreamFrame = serde_json::from_str(payload).unwrap();
        let d = f.choices[0].delta.as_ref().unwrap();
        assert!(d.content.is_none());
        let tc = d.tool_calls.as_ref().unwrap();
        assert_eq!(tc[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn message_json_restores_multimodal_and_tool_fields() {
        let m = crate::engine::ChatMessage {
            role: "user".into(),
            content: String::new(),
            content_json: r#"[{"type":"text","text":"what is this?"},{"type":"image_url","image_url":{"url":"https://x/y.png"}}]"#.into(),
            tool_calls_json: String::new(),
            tool_call_id: String::new(),
        };
        let v = message_json(&m);
        assert!(v["content"].is_array());
        assert_eq!(v["content"][1]["type"], "image_url");

        let t = crate::engine::ChatMessage {
            role: "tool".into(),
            content: "72F".into(),
            content_json: String::new(),
            tool_calls_json: String::new(),
            tool_call_id: "call_1".into(),
        };
        let v = message_json(&t);
        assert_eq!(v["content"], "72F");
        assert_eq!(v["tool_call_id"], "call_1");
    }

    #[test]
    fn frame_end_handles_lf_and_crlf() {
        assert_eq!(find_frame_end(b"data: x\n\nrest"), Some(9));
        assert_eq!(find_frame_end(b"data: x\r\n\r\nrest"), Some(11));
        assert_eq!(find_frame_end(b"data: x\n"), None);
        assert_eq!(find_frame_end(b"data: x\r\n"), None);
        assert_eq!(find_frame_end(b""), None);
    }

    #[test]
    fn parses_finish_only_terminator() {
        let payload = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let f: StreamFrame = serde_json::from_str(payload).unwrap();
        let c = &f.choices[0];
        assert_eq!(c.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn kv_transfer_params_from_digests_hex_encodes() {
        let d = vec![0u8; 32];
        let r = vec![1u8; 32];
        let v = kv_transfer_params_from_digests(std::slice::from_ref(&d), std::slice::from_ref(&r))
            .unwrap();
        let arr = v["cgn_prefix_digests"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str().unwrap().len(), 64);
        let resident = v["cgn_resident_digests"].as_array().unwrap();
        assert_eq!(resident.len(), 1);
        assert!(kv_transfer_params_from_digests(&[], &[]).is_none());
        assert!(kv_transfer_params_from_digests(&[vec![1, 2, 3]], &[]).is_none());
        let v2 = kv_transfer_params_from_digests(&[d], &[]).unwrap();
        assert!(v2.get("cgn_resident_digests").is_none());
    }
}

#[cfg(test)]
mod sampling_tests {
    use super::*;

    fn req() -> GenerateReq {
        GenerateReq {
            id: "r".into(),
            model: "m".into(),
            messages: vec![],
            prompt: "p".into(),
            max_tokens: 0,
            temperature: 1.0,
            top_p: 1.0,
            stop: vec![],
            stream: true,
            seed: None,
            frequency_penalty: 0.0,
            presence_penalty: 0.0,
            repetition_penalty: 1.0,
            top_k: 0,
            logprobs: false,
            extensions_json: String::new(),
            prefix_digests: vec![],
            resident_digests: vec![],
        }
    }

    #[test]
    fn unset_fields_are_omitted_so_engine_defaults_apply() {
        let mut b = serde_json::json!({});
        apply_sampling(&mut b, &req(), true);
        for k in [
            "max_tokens",
            "seed",
            "frequency_penalty",
            "presence_penalty",
            "top_k",
            "logprobs",
        ] {
            assert!(b.get(k).is_none(), "{k} should be omitted");
        }
        assert_eq!(b["stream_options"]["include_usage"], true);
    }

    #[test]
    fn client_sampling_reaches_the_engine() {
        let mut r = req();
        r.max_tokens = 2000;
        r.seed = Some(0);
        r.frequency_penalty = 0.5;
        r.presence_penalty = -0.5;
        r.repetition_penalty = 1.1;
        r.top_k = 40;
        r.logprobs = true;
        let mut b = serde_json::json!({});
        apply_sampling(&mut b, &r, true);
        assert_eq!(b["max_tokens"], 2000);
        assert_eq!(b["seed"], 0, "seed 0 is a real seed");
        assert_eq!(b["frequency_penalty"], 0.5);
        assert_eq!(b["presence_penalty"], -0.5);
        assert_eq!(b["top_k"], 40);
        assert_eq!(b["logprobs"], true);
    }

    #[test]
    fn legacy_completions_take_a_logprob_count() {
        let mut r = req();
        r.logprobs = true;
        let mut b = serde_json::json!({});
        apply_sampling(&mut b, &r, false);
        assert_eq!(b["logprobs"], 1);
    }

    #[test]
    fn engine_errors_classify_for_retry() {
        use reqwest::StatusCode as S;
        let c = |s| classify_engine_status("vllm", s, "x");
        assert!(matches!(c(S::BAD_REQUEST), Error::InvalidArgument(_)));
        assert!(matches!(c(S::TOO_MANY_REQUESTS), Error::Unavailable(_)));
        assert!(matches!(c(S::SERVICE_UNAVAILABLE), Error::Unavailable(_)));
        assert!(matches!(c(S::INTERNAL_SERVER_ERROR), Error::Internal(_)));
    }

    #[test]
    fn logprobs_parse_from_both_shapes() {
        let chat = serde_json::json!({"content":[{"logprob":-1.0},{"logprob":-3.0}]});
        assert_eq!(mean_logprob(&chat), Some(-2.0));
        let legacy = serde_json::json!({"token_logprobs":[-0.5, null, -1.5]});
        assert_eq!(mean_logprob(&legacy), Some(-1.0));
        assert_eq!(mean_logprob(&serde_json::json!({"content":[]})), None);
    }

    #[test]
    fn terminal_token_never_doubles_the_finish_reason() {
        assert_eq!(terminal_token("r", true, (3, 4)).finish, "");
        let t = terminal_token("r", false, (3, 4));
        assert_eq!(
            (t.finish.as_str(), t.prompt_tokens, t.completion_tokens),
            ("stop", 3, 4)
        );
    }
}
