# OpenAI-compatible HTTP surface

`cgn-router` listens on `[router].listen_http` (default `:8080`) and
speaks the OpenAI HTTP/SSE protocol. Any OpenAI SDK can target it
unchanged — point `OPENAI_BASE_URL` at the router and use any API
key from `cgn-ctl key create`.

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://router.cognitora.local:8080/v1",
    api_key="cgn-c782d73a8c914c3da49191626f95737e",
)

stream = client.chat.completions.create(
    model="llama3-8b",
    messages=[{"role": "user", "content": "Explain KV-aware routing in one sentence."}],
    stream=True,
)
for chunk in stream:
    print(chunk.choices[0].delta.content or "", end="", flush=True)
```

## Endpoints

| Method | Path                       | Status        | Notes                                             |
|--------|----------------------------|---------------|---------------------------------------------------|
| `POST` | `/v1/chat/completions`     | implemented   | streaming + buffered                              |
| `POST` | `/v1/completions`          | implemented   | alias of `chat/completions` for legacy SDKs       |
| `POST` | `/v1/embeddings`           | placeholder   | returns deterministic vectors until `Agent.Embed` |
| `GET`  | `/v1/models`               | implemented   | union of `[models.*]` config + live agents        |
| `GET`  | `/healthz`                 | implemented   | liveness probe (admin port)                       |
| `GET`  | `/readyz`                  | implemented   | readiness probe (admin port)                      |

Not yet implemented (tracked, no committed timeline): assistants /
threads, tool calls, fine-tunes, audio, image, batch.

## Auth

Every `/v1/*` request goes through `cgn-auth::middleware`. Two flows:

- **API key** — `Authorization: Bearer cgn-<32hex>`. The token's
  sha256 is matched against `[auth].api_keys_file`. Use
  `cgn-ctl key create --scopes "chat,embed"` to issue one. Tokens are
  shown once; the file stores hashes.
- **OIDC** — same header but with a JWT. `cgn-auth` validates the
  signature against the issuer's JWKS (rotated every
  `[auth].oidc_jwks_ttl`, default 10m). The `sub` claim becomes the
  rate-limit subject.

When `[auth].enabled = false` the middleware is a no-op (CI / dev only).

## Streaming

Server-Sent Events (`text/event-stream`). Each token chunk is one
`data: {...}\n\n` line followed by `data: [DONE]\n\n`. The shape
matches OpenAI exactly:

```json
data: {"id":"chatcmpl-…","object":"chat.completion.chunk","created":1714566600,"model":"llama3-8b","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}
data: {"id":"chatcmpl-…","object":"chat.completion.chunk","created":1714566600,"model":"llama3-8b","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}
data: [DONE]
```

The router never buffers a streaming response — tokens flow directly
from `Agent.Generate` through `gateway::sse::SseEncoder` to the
client.

## Error envelope

All errors return the OpenAI-shaped JSON:

```json
{ "error": { "code": null, "message": "<text>", "type": "<type>" } }
```

| HTTP | `type`             | When                                                     |
|-----:|--------------------|----------------------------------------------------------|
| 401  | `auth_error`       | missing / invalid bearer                                 |
| 403  | `auth_error`       | valid token but scope insufficient                       |
| 404  | `model_not_found`  | unknown model name                                       |
| 429  | `rate_limit`       | `cgn-ratelimit` quota exhausted                          |
| 429  | `server_error`     | router admission queue full (`max_queue` reached)        |
| 503  | `server_error`     | no live node serving the model for the requested role    |
| 5xx  | `server_error`     | upstream failure (engine crash, transport)               |

## Headers

`cgn-router` honours and emits a small set of beyond-spec headers:

| Header                   | Direction | Purpose                                              |
|--------------------------|-----------|------------------------------------------------------|
| `x-request-id`           | both      | propagated to traces and `cgn-agent` logs            |
| `x-cgn-subject`          | inbound\* | set by `cgn-auth`; downstream rate limit reads this  |
| `x-cgn-cache-hit`        | outbound  | `true`/`false` — was the prefix found in `cgn-kvcached` |
| `x-cgn-node`             | outbound  | which node id served the request                     |
| `x-cgn-cascade-step`     | outbound  | which model in a cascade chain produced the response |

\* set internally by middleware; an inbound request setting
`x-cgn-subject` directly is rejected.

## OpenAPI

A machine-readable spec (`openapi.yaml`) lives next to this doc and is
regenerated from `cgn-router::gateway::types` on every release. Use it
with `openapi-generator-cli` to scaffold typed clients in any
language.
