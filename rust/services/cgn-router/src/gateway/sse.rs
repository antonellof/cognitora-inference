//! Tiny SSE encoder used by the streaming endpoints.
//!
//! OpenAI clients expect `data: <json>\n\n` frames and a final
//! `data: [DONE]\n\n` sentinel. A custom body keeps the shape explicit.
//!
//! `[DONE]` means success, so a stream that fails sends an error event and
//! ends without it. SDKs then raise instead of returning a truncated
//! answer as if it were complete.

use std::convert::Infallible;

use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use futures::stream::Stream;
use futures::StreamExt;

/// One item on the channel into the response body.
#[derive(Debug, PartialEq, Eq)]
pub enum SseEvent {
    Data(String),
    /// Terminal error frame; suppresses `[DONE]`.
    Fail(String),
}

fn frame(json: &str) -> Bytes {
    let mut buf = String::with_capacity(json.len() + 8);
    buf.push_str("data: ");
    buf.push_str(json);
    buf.push_str("\n\n");
    Bytes::from(buf)
}

/// Frame events, appending `[DONE]` unless the stream failed.
pub fn encode<S>(events: S) -> impl Stream<Item = Bytes>
where
    S: Stream<Item = SseEvent> + Send + 'static,
{
    futures::stream::unfold(
        (Box::pin(events), false, false),
        |(mut events, failed, finished)| async move {
            if finished {
                return None;
            }
            match events.next().await {
                Some(SseEvent::Data(json)) => Some((frame(&json), (events, failed, false))),
                Some(SseEvent::Fail(json)) => Some((frame(&json), (events, true, true))),
                None if failed => None,
                None => Some((
                    Bytes::from_static(b"data: [DONE]\n\n"),
                    (events, failed, true),
                )),
            }
        },
    )
}

/// Wrap a stream of events into an SSE response.
pub fn into_response<S>(stream: S) -> Response
where
    S: Stream<Item = SseEvent> + Send + 'static,
{
    use axum::http::header;

    let body = encode(stream).map(Ok::<Bytes, Infallible>);
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(body))
        .expect("sse response build")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body(events: Vec<SseEvent>) -> String {
        let chunks: Vec<Bytes> = encode(futures::stream::iter(events)).collect().await;
        chunks
            .iter()
            .map(|b| std::str::from_utf8(b).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn success_ends_with_done() {
        let b = body(vec![SseEvent::Data("{}".into())]).await;
        assert_eq!(b, "data: {}\n\ndata: [DONE]\n\n");
    }

    #[tokio::test]
    async fn failure_ends_without_done() {
        let b = body(vec![
            SseEvent::Data("{}".into()),
            SseEvent::Fail("{\"error\":{}}".into()),
        ])
        .await;
        assert!(b.ends_with("data: {\"error\":{}}\n\n"));
        assert!(!b.contains("[DONE]"));
    }
}
