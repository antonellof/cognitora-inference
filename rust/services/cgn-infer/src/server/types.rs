//! OpenAI wire types for `/v1/chat/completions` and
//! `/v1/completions`, matching what `cgn-agent`'s `OpenAiHttpEngine`
//! sends and expects back (streaming deltas in `choices[].delta
//! .content` for chat, `choices[].text` for legacy completions,
//! `data: [DONE]` terminator).

use serde::{Deserialize, Serialize};

use crate::model::ChatMessage;
use crate::sampling::SamplingParams;

fn default_max_tokens() -> usize {
    512
}

/// `stop` may be a single string or an array of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StopField {
    One(String),
    Many(Vec<String>),
}

impl StopField {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            StopField::One(s) => vec![s],
            StopField::Many(v) => v,
        }
    }
}

/// `prompt` may be a string or an array (we join arrays, matching
/// llama.cpp's behavior for the common single-element case).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PromptField {
    One(String),
    Many(Vec<String>),
}

impl PromptField {
    pub fn into_string(self) -> String {
        match self {
            PromptField::One(s) => s,
            PromptField::Many(v) => v.join(""),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    #[serde(default)]
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub repetition_penalty: Option<f32>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub stop: Option<StopField>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Deserialize)]
pub struct CompletionRequest {
    #[serde(default)]
    pub model: Option<String>,
    pub prompt: PromptField,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub repetition_penalty: Option<f32>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub stop: Option<StopField>,
    #[serde(default)]
    pub stream: bool,
}

/// Build [`SamplingParams`] from the optional request knobs.
pub fn sampling_params(
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<usize>,
    repetition_penalty: Option<f32>,
    seed: Option<u64>,
) -> SamplingParams {
    let d = SamplingParams::default();
    SamplingParams {
        temperature: temperature.unwrap_or(d.temperature).max(0.0),
        top_p: top_p.unwrap_or(d.top_p).clamp(f32::MIN_POSITIVE, 1.0),
        top_k: top_k.unwrap_or(d.top_k),
        repetition_penalty: repetition_penalty.unwrap_or(d.repetition_penalty).max(0.0),
        seed,
        ..d
    }
}

// ---------------------------------------------------------------- responses

#[derive(Debug, Serialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str, // "chat.completion"
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Usage,
}

#[derive(Debug, Serialize)]
pub struct ChatChoice {
    pub index: usize,
    pub message: ChatMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct ChatChunk {
    pub id: String,
    pub object: &'static str, // "chat.completion.chunk"
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChunkChoice>,
}

#[derive(Debug, Serialize)]
pub struct ChatChunkChoice {
    pub index: usize,
    pub delta: ChatDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct ChatDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: &'static str, // "text_completion"
    pub created: i64,
    pub model: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: Usage,
}

#[derive(Debug, Serialize)]
pub struct CompletionChoice {
    pub index: usize,
    pub text: String,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct CompletionChunk {
    pub id: String,
    pub object: &'static str, // "text_completion"
    pub created: i64,
    pub model: String,
    pub choices: Vec<CompletionChunkChoice>,
}

#[derive(Debug, Serialize)]
pub struct CompletionChunkChoice {
    pub index: usize,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ModelList {
    pub object: &'static str, // "list"
    pub data: Vec<ModelEntry>,
}

#[derive(Debug, Serialize)]
pub struct ModelEntry {
    pub id: String,
    pub object: &'static str, // "model"
    pub created: i64,
    pub owned_by: &'static str,
}

/// OpenAI-shaped error body.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub message: String,
    pub r#type: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_chat_request() {
        let req: ChatCompletionRequest =
            serde_json::from_str(r#"{"messages":[{"role":"user","content":"hi"}]}"#).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.max_tokens, 512);
        assert!(!req.stream);
        assert!(req.stop.is_none());
    }

    #[test]
    fn parses_agent_shaped_chat_request() {
        // Exactly the body cgn-agent's OpenAiHttpEngine sends.
        let req: ChatCompletionRequest = serde_json::from_str(
            r#"{"model":"m","messages":[{"role":"user","content":"hi"}],
                "max_tokens":128,"temperature":0.7,"top_p":0.9,
                "stop":["\n\n"],"stream":true}"#,
        )
        .unwrap();
        assert!(req.stream);
        assert_eq!(req.stop.unwrap().into_vec(), vec!["\n\n"]);
        assert_eq!(req.temperature, Some(0.7));
    }

    #[test]
    fn stop_accepts_string_or_array() {
        let one: ChatCompletionRequest =
            serde_json::from_str(r#"{"messages":[],"stop":"END"}"#).unwrap();
        assert_eq!(one.stop.unwrap().into_vec(), vec!["END"]);
        let many: ChatCompletionRequest =
            serde_json::from_str(r#"{"messages":[],"stop":["a","b"]}"#).unwrap();
        assert_eq!(many.stop.unwrap().into_vec(), vec!["a", "b"]);
    }

    #[test]
    fn prompt_accepts_string_or_array() {
        let one: CompletionRequest = serde_json::from_str(r#"{"prompt":"hello"}"#).unwrap();
        assert_eq!(one.prompt.into_string(), "hello");
        let many: CompletionRequest = serde_json::from_str(r#"{"prompt":["a","b"]}"#).unwrap();
        assert_eq!(many.prompt.into_string(), "ab");
    }

    #[test]
    fn null_optionals_are_tolerated() {
        // Clients (incl. cgn-agent) send explicit nulls for unset knobs.
        let req: CompletionRequest =
            serde_json::from_str(r#"{"prompt":"p","temperature":null,"top_p":null,"stop":null}"#)
                .unwrap();
        assert!(req.temperature.is_none());
        assert!(req.stop.is_none());
    }

    #[test]
    fn sampling_params_clamp() {
        let p = sampling_params(Some(-1.0), Some(2.0), None, None, Some(7));
        assert_eq!(p.temperature, 0.0);
        assert_eq!(p.top_p, 1.0);
        assert_eq!(p.seed, Some(7));
    }

    #[test]
    fn chat_chunk_serializes_like_openai() {
        let chunk = ChatChunk {
            id: "chatcmpl-1".into(),
            object: "chat.completion.chunk",
            created: 0,
            model: "m".into(),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: Some("hi".into()),
                },
                finish_reason: None,
            }],
        };
        let json = serde_json::to_string(&chunk).unwrap();
        // The exact shape cgn-agent's StreamFrame parser consumes.
        assert!(json.contains(r#""delta":{"content":"hi"}"#));
        assert!(!json.contains("finish_reason"));
    }
}
