//! axum HTTP server exposing the OpenAI-compatible surface:
//!
//! * `POST /v1/chat/completions` — buffered or SSE streaming
//! * `POST /v1/completions`      — buffered or SSE streaming
//! * `GET  /v1/models`
//! * `GET  /healthz` (and `/health`, which cgn-agent probes)

pub mod types;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cgn_core::{Error, Result};
use futures::Stream;
use tokio::sync::mpsc;
use futures::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;

use crate::engine::{Engine, GenerateRequest, StreamEvent};
use crate::model::ChatMessage;
use types::*;

pub struct AppState {
    pub engine: Engine,
}

pub async fn serve(engine: Engine, addr: SocketAddr) -> Result<()> {
    let state = Arc::new(AppState { engine });
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/models", get(models))
        .route("/healthz", get(healthz))
        .route("/health", get(healthz))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| Error::Config(format!("bind {addr}: {e}")))?;
    info!(%addr, "cgn-infer listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| Error::Internal(format!("http server: {e}")))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn models(State(state): State<Arc<AppState>>) -> Json<ModelList> {
    Json(ModelList {
        object: "list",
        data: vec![ModelEntry {
            id: state.engine.model_id.clone(),
            object: "model",
            created: chrono::Utc::now().timestamp(),
            owned_by: "cognitora",
        }],
    })
}

/// Map engine errors onto OpenAI-shaped error responses.
struct ApiError(Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, r#type) = match &self.0 {
            Error::InvalidArgument(_) | Error::Config(_) | Error::Json(_) => {
                (StatusCode::BAD_REQUEST, "invalid_request_error")
            }
            Error::NotFound(_) => (StatusCode::NOT_FOUND, "invalid_request_error"),
            Error::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "server_error"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "server_error"),
        };
        (
            status,
            Json(ErrorResponse {
                error: ErrorBody {
                    message: self.0.to_string(),
                    r#type,
                },
            }),
        )
            .into_response()
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        Self(e)
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Start generation and hand back the event receiver.
fn start_generation(state: Arc<AppState>, req: GenerateRequest) -> mpsc::Receiver<StreamEvent> {
    let (tx, rx) = mpsc::channel(64);
    let engine_tx = tx.clone();
    // Errors after streaming has begun can't change the HTTP status;
    // surface them as a terminal frame so clients see a finish.
    let fut = async move {
        if let Err(e) = state.engine.generate(req, engine_tx).await {
            tracing::error!(error = %e, "generation failed");
            let _ = tx
                .send(StreamEvent {
                    delta: String::new(),
                    finish_reason: Some("error".into()),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                })
                .await;
        }
    };
    tokio::spawn(fut);
    rx
}

/// Drain the whole stream for buffered (non-`stream`) responses.
async fn collect(mut rx: mpsc::Receiver<StreamEvent>) -> Result<(String, String, usize, usize)> {
    let mut text = String::new();
    let (mut finish, mut pt, mut ct) = ("stop".to_string(), 0, 0);
    while let Some(ev) = rx.recv().await {
        text.push_str(&ev.delta);
        pt = ev.prompt_tokens;
        ct = ev.completion_tokens;
        if let Some(f) = ev.finish_reason {
            finish = f;
            break;
        }
    }
    if finish == "error" {
        return Err(Error::Internal("generation failed".into()));
    }
    Ok((text, finish, pt, ct))
}

async fn chat_completions(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatCompletionRequest>,
) -> std::result::Result<Response, ApiError> {
    if req.messages.is_empty() {
        return Err(ApiError(Error::InvalidArgument(
            "messages must not be empty".into(),
        )));
    }
    let prompt = state.engine.render_chat(&req.messages)?;
    let gen = GenerateRequest {
        prompt,
        max_tokens: req.max_tokens,
        stop: req.stop.map(StopField::into_vec).unwrap_or_default(),
        params: sampling_params(req.temperature, req.top_p, req.top_k, req.repetition_penalty, req.seed),
    };
    let model = state.engine.model_id.clone();
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let rx = start_generation(state, gen);

    if req.stream {
        return Ok(sse_response(chat_sse_stream(id, model, rx)));
    }

    let (content, finish, pt, ct) = collect(rx).await?;
    Ok(Json(ChatCompletionResponse {
        id,
        object: "chat.completion",
        created: now(),
        model,
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content,
            },
            finish_reason: finish,
        }],
        usage: Usage {
            prompt_tokens: pt,
            completion_tokens: ct,
            total_tokens: pt + ct,
        },
    })
    .into_response())
}

async fn completions(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CompletionRequest>,
) -> std::result::Result<Response, ApiError> {
    let gen = GenerateRequest {
        prompt: req.prompt.into_string(),
        max_tokens: req.max_tokens,
        stop: req.stop.map(StopField::into_vec).unwrap_or_default(),
        params: sampling_params(req.temperature, req.top_p, req.top_k, req.repetition_penalty, req.seed),
    };
    let model = state.engine.model_id.clone();
    let id = format!("cmpl-{}", uuid::Uuid::new_v4().simple());
    let rx = start_generation(state, gen);

    if req.stream {
        return Ok(sse_response(completion_sse_stream(id, model, rx)));
    }

    let (text, finish, pt, ct) = collect(rx).await?;
    Ok(Json(CompletionResponse {
        id,
        object: "text_completion",
        created: now(),
        model,
        choices: vec![CompletionChoice {
            index: 0,
            text,
            finish_reason: finish,
        }],
        usage: Usage {
            prompt_tokens: pt,
            completion_tokens: ct,
            total_tokens: pt + ct,
        },
    })
    .into_response())
}

fn sse_response(
    stream: impl Stream<Item = std::result::Result<Event, Infallible>> + Send + 'static,
) -> Response {
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn json_event<T: serde::Serialize>(v: &T) -> Event {
    Event::default().data(serde_json::to_string(v).expect("serializable chunk"))
}

fn chat_sse_stream(
    id: String,
    model: String,
    rx: mpsc::Receiver<StreamEvent>,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    let created = now();
    // OpenAI chat streams open with a role-only chunk.
    let first = ChatChunk {
        id: id.clone(),
        object: "chat.completion.chunk",
        created,
        model: model.clone(),
        choices: vec![ChatChunkChoice {
            index: 0,
            delta: ChatDelta {
                role: Some("assistant".into()),
                content: None,
            },
            finish_reason: None,
        }],
    };
    let head = futures::stream::once(async move { Ok(json_event(&first)) });

    let body = ReceiverStream::new(rx).map(move |ev| {
        let done = ev.finish_reason.is_some();
        let chunk = ChatChunk {
            id: id.clone(),
            object: "chat.completion.chunk",
            created,
            model: model.clone(),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: (!ev.delta.is_empty()).then_some(ev.delta),
                },
                finish_reason: ev.finish_reason,
            }],
        };
        let mut out = vec![Ok(json_event(&chunk))];
        if done {
            out.push(Ok(Event::default().data("[DONE]")));
        }
        futures::stream::iter(out)
    });
    head.chain(body.flatten())
}

fn completion_sse_stream(
    id: String,
    model: String,
    rx: mpsc::Receiver<StreamEvent>,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    let created = now();
    ReceiverStream::new(rx)
        .map(move |ev| {
            let done = ev.finish_reason.is_some();
            let chunk = CompletionChunk {
                id: id.clone(),
                object: "text_completion",
                created,
                model: model.clone(),
                choices: vec![CompletionChunkChoice {
                    index: 0,
                    text: ev.delta,
                    finish_reason: ev.finish_reason,
                }],
            };
            let mut out = vec![Ok(json_event(&chunk))];
            if done {
                out.push(Ok(Event::default().data("[DONE]")));
            }
            futures::stream::iter(out)
        })
        .flatten()
}
