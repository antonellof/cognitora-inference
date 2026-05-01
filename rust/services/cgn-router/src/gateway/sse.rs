//! Tiny SSE encoder used by the streaming chat endpoint.
//!
//! OpenAI clients expect the `data: <json>\n\n` framing followed by a final
//! `data: [DONE]\n\n` sentinel. Using axum's `sse` helpers directly works
//! but ties our types to an unstable surface; a custom Body keeps the
//! response shape explicit.

use std::convert::Infallible;

use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use futures::stream::Stream;
use futures::StreamExt;

/// Wrap a stream of pre-serialised JSON strings into an SSE response.
pub fn into_response<S>(stream: S) -> Response
where
    S: Stream<Item = String> + Send + 'static,
{
    use axum::http::header;

    let framed = stream
        .map(|chunk| {
            let mut buf = String::with_capacity(chunk.len() + 8);
            buf.push_str("data: ");
            buf.push_str(&chunk);
            buf.push_str("\n\n");
            Ok::<Bytes, Infallible>(Bytes::from(buf))
        })
        .chain(futures::stream::once(async {
            Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"))
        }));

    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(framed))
        .expect("sse response build")
}
