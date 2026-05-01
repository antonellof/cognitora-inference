//! vLLM engine driver.
//!
//! Talks to a locally-spawned vLLM process via its OpenAI-compatible HTTP
//! surface. Streaming generation uses vLLM's SSE response on `/v1/completions`.

use std::time::Duration;

use async_trait::async_trait;
use cgn_core::{Error, Result};
use cgn_proto::v1::Token;
use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::warn;

use super::{Engine, GenerateReq, ModelSpec};

pub struct VllmEngine {
    client: reqwest::Client,
    base:   String,
}

impl VllmEngine {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600)) // long-running streams
            .pool_max_idle_per_host(8)
            .build()
            .map_err(|e| Error::Internal(format!("reqwest: {e}")))?;
        Ok(Self { client, base: base_url.into().trim_end_matches('/').to_string() })
    }
}

#[async_trait]
impl Engine for VllmEngine {
    fn name(&self) -> &'static str { "vllm" }

    async fn load_model(&self, _spec: ModelSpec) -> Result<()> {
        // vLLM loads its model when spawned; the supervisor handles process
        // lifecycle. This call exists to support future engines (SGLang)
        // that accept dynamic model swaps over their control plane.
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

        let resp = self.client.post(&url).json(&body).send().await
            .map_err(|e| Error::Unavailable(format!("vllm post: {e}")))?;

        if !resp.status().is_success() {
            let s = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            return Err(Error::Internal(format!("vllm status {s}: {txt}")));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = Vec::with_capacity(8192);
        let id = req.id.clone();

        while let Some(item) = stream.next().await {
            let bytes = item.map_err(|e| Error::Internal(format!("vllm stream: {e}")))?;
            buf.extend_from_slice(&bytes);
            // SSE frames are `data: <json>\n\n`. Pop complete frames.
            while let Some(idx) = find_subseq(&buf, b"\n\n") {
                let frame = buf.drain(..idx + 2).collect::<Vec<u8>>();
                let line = std::str::from_utf8(&frame).unwrap_or("").trim();
                if !line.starts_with("data:") { continue; }
                let payload = line.trim_start_matches("data:").trim();
                if payload == "[DONE]" {
                    let _ = tx.send(Token {
                        id: id.clone(), text: String::new(),
                        token_id: 0, logprob: 0.0,
                        finish: "stop".into(), prefix_hash: vec![],
                    }).await;
                    return Ok(());
                }
                match serde_json::from_str::<VllmStreamFrame>(payload) {
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
                    Err(e) => warn!(error=?e, payload, "skipping unparsable vllm frame"),
                }
            }
        }
        Ok(())
    }

    async fn ready(&self) -> bool {
        let url = format!("{}/health", self.base);
        matches!(
            self.client.get(&url).timeout(Duration::from_secs(2)).send().await,
            Ok(r) if r.status().is_success()
        )
    }
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[derive(Deserialize)]
struct VllmStreamFrame {
    choices: Vec<VllmChoice>,
}

#[derive(Deserialize)]
struct VllmChoice {
    #[serde(default)] text: Option<String>,
    #[serde(default)] finish_reason: Option<String>,
}
