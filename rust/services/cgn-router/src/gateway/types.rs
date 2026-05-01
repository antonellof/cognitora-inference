//! OpenAI-compatible request/response shapes.
//!
//! Only fields Cognitora actually uses are deserialised; unknown fields are
//! tolerated for forward-compat with newer SDKs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub stop: Option<StopSpec>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub seed: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum StopSpec {
    Single(String),
    Multi(Vec<String>),
}
impl StopSpec {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            StopSpec::Single(s) => vec![s],
            StopSpec::Multi(v)  => v,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Usage,
}

#[derive(Debug, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize, Default)]
pub struct Usage {
    pub prompt_tokens:     u32,
    pub completion_tokens: u32,
    pub total_tokens:      u32,
}

// Streaming SSE chunk shape.
#[derive(Debug, Serialize)]
pub struct ChatChunk {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChunkChoice>,
}

#[derive(Debug, Serialize)]
pub struct ChatChunkChoice {
    pub index: u32,
    pub delta: ChatDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct ChatDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role:    Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ---------------------------------------------------------------------------
// Embeddings
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct EmbedRequest {
    pub model: String,
    #[serde(default)]
    pub input: EmbedInput,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
pub enum EmbedInput {
    Single(String),
    Multi(Vec<String>),
    #[default]
    Empty,
}
impl EmbedInput {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            EmbedInput::Single(s) => vec![s],
            EmbedInput::Multi(v)  => v,
            EmbedInput::Empty     => vec![],
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EmbedResponse {
    pub object: &'static str, // "list"
    pub data: Vec<EmbedItem>,
    pub model: String,
    pub usage: Usage,
}

#[derive(Debug, Serialize)]
pub struct EmbedItem {
    pub object: &'static str, // "embedding"
    pub index: u32,
    pub embedding: Vec<f32>,
}

// ---------------------------------------------------------------------------
// Models list
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ModelsResponse {
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
