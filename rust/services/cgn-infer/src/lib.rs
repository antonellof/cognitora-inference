//! `cgn-infer` — Cognitora's native inference engine.
//!
//! Loads quantized Llama-family GGUF models via mmap, runs them with
//! Candle (CPU always; Metal/CUDA behind cargo features), and serves
//! the OpenAI-compatible HTTP/SSE surface the rest of the platform
//! already speaks (`/v1/chat/completions`, `/v1/completions`,
//! `/v1/models`, `/healthz`).
//!
//! Requests flow through the continuous-batching [`scheduler`]
//! (phase 3): `llama`/`qwen2` GGUFs decode several sequences per
//! step through the crate's own batched forward pass
//! ([`model::llama`]); other supported architectures (`qwen3`,
//! `gemma3`, `phi3`, MoE llama) fall back to a sequential runtime.
//! Distributed layer-pipeline inference (phase 4) lives in
//! [`pipeline`]: `cgn-infer worker --layers A:B` serves a layer
//! slice over gRPC and `cgn-infer serve --role coordinator` drives
//! the pipeline.

pub mod engine;
pub mod kv;
pub mod model;
pub mod pipeline;
pub mod runtime;
pub mod sampling;
pub mod scheduler;
pub mod server;

pub use cgn_core::{Error, Result};
