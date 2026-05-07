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
            .timeout(Duration::from_secs(600)) // long-running streams
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
        let url = format!("{}/v1/completions", self.base);

        let body = serde_json::json!({
            "model":       req.model,
            "prompt":      req.prompt,
            "max_tokens":  req.max_tokens,
            "temperature": req.temperature,
            "top_p":       req.top_p,
            "stop":        req.stop,
            "stream":      true,
        });

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
            return Err(Error::Internal(format!("{} status {s}: {txt}", self.kind)));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = Vec::with_capacity(8192);
        let id = req.id.clone();

        while let Some(item) = stream.next().await {
            let bytes = item.map_err(|e| Error::Internal(format!("{} stream: {e}", self.kind)))?;
            buf.extend_from_slice(&bytes);
            // SSE frames are `data: <json>\n\n`. Pop complete frames.
            while let Some(idx) = find_subseq(&buf, b"\n\n") {
                let frame = buf.drain(..idx + 2).collect::<Vec<u8>>();
                let line = std::str::from_utf8(&frame).unwrap_or("").trim();
                if !line.starts_with("data:") {
                    continue;
                }
                let payload = line.trim_start_matches("data:").trim();
                if payload == "[DONE]" {
                    let _ = tx
                        .send(Token {
                            id: id.clone(),
                            text: String::new(),
                            token_id: 0,
                            logprob: 0.0,
                            finish: "stop".into(),
                            prefix_hash: vec![],
                        })
                        .await;
                    return Ok(());
                }
                match serde_json::from_str::<StreamFrame>(payload) {
                    Ok(f) => {
                        for choice in f.choices {
                            let token = Token {
                                id: id.clone(),
                                text: choice.text.unwrap_or_default(),
                                token_id: 0,
                                logprob: 0.0,
                                finish: choice.finish_reason.unwrap_or_default(),
                                prefix_hash: vec![],
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

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[derive(Deserialize)]
struct StreamFrame {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct EmbedFrame {
    data: Vec<EmbedDatum>,
    #[serde(default)]
    usage: Option<EmbedUsage>,
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
