# OpenAI-compatible HTTP surface

`cgn-router` listens on `[router].listen_http` (default `:8080`) and
speaks the OpenAI HTTP/SSE protocol. Any OpenAI SDK can target it
unchanged: point `OPENAI_BASE_URL` at the router and use any API
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
| `POST` | `/v1/completions`          | implemented   | legacy raw `prompt`, no chat template, one prompt per call |
| `POST` | `/v1/embeddings`           | implemented   | KV-aware routing → `Agent.Embed` → engine `/v1/embeddings` round-trip |
| `GET`  | `/v1/models`               | implemented   | union of `[models.*]` config + live agents        |
| `GET`  | `/v1/usage`                | implemented   | the caller's token totals and limits (needs auth) |
| `GET`  | `/healthz`                 | implemented   | liveness probe (admin port)                       |
| `GET`  | `/readyz`                  | implemented   | 503 while draining or with no ready node          |

Not yet implemented (tracked, no committed timeline): assistants /
threads, fine-tunes, audio, image generation, batch, `/v1/responses`.

## Request fields

Forwarded to the engine: `temperature`, `top_p`, `top_k`, `min_p`, `stop`,
`max_tokens`, `max_completion_tokens` (wins if both are set), `seed`,
`frequency_penalty`, `presence_penalty`, `repetition_penalty`, `logit_bias`,
`logprobs`, `top_logprobs`, `tools`, `tool_choice`, `parallel_tool_calls`,
`response_format`, and image content parts. `stream_options.include_usage`
adds a final chunk with empty `choices` and the usage.

`n` must be 1; anything else is a 400 rather than silently ignored. With no
`max_tokens`, the engine's default applies. `usage` comes from the engine, or
from a router estimate for engines that don't report it.

Optional request headers:

| Header | Effect |
|--------|--------|
| `x-session-id` | Keep a conversation on one node. Falls back to the `user` field. |
| `x-cgn-deadline-ms` | Soft latency budget for deadline admission. |
| `x-cgn-priority: low` | Eligible for carbon-aware deferral. |
| `traceparent`, `tracestate` | W3C trace context, forwarded to agents. |

A LoRA adapter from `[models.*.lora]` is requested by using its name as
`model`. `/v1/models` lists it with its `parent`.

## Auth

Every `/v1/*` request goes through `cgn-auth::middleware`. Two flows:

- **API key**: `Authorization: Bearer cgn-<32hex>`. The token's
  sha256 is matched against `[auth].api_keys_file`. Use
  `cgn-ctl key create --scopes "chat,embed"` to issue one. Tokens are
  shown once; the file stores hashes. Changes apply within 15 s.
- **OIDC**: same header but with a JWT. `cgn-auth` validates the
  signature against the issuer's JWKS (rotated every
  `[auth].oidc_jwks_ttl`, default 10m). The `sub` claim becomes the
  rate-limit subject. `[auth].oidc_group_scopes` maps IdP groups to scopes.

When `[auth].enabled = false` the middleware is a no-op (CI / dev only).

### Scopes

Requests are refused with 403 unless the credential grants them. Set
`[auth].enforce_scopes = false` only while migrating existing keys.

| Scope | Grants |
|-------|--------|
| `chat`, `embed` | `/v1/chat/completions` + `/v1/completions`, `/v1/embeddings` |
| `*` | every endpoint and model |
| `model:<name>` | restrict to this model; repeatable, `model:qwen*` matches a prefix |
| `tenant:<id>` | bill and isolate as this tenant (default: the key or subject) |
| `tpm:<n>` | at most n tokens per minute, 429 with `Retry-After` beyond it |
| `budget:<n>` | at most n tokens ever, 429 with `insufficient_quota` beyond it |

Scopes make a key a virtual key:

```bash
cgn-ctl key create --scopes "chat,model:llama3-8b,tenant:acme,tpm:60000,budget:5000000"
```

Limits are enforced per router, and a request in flight can overshoot by its
own size. Without `[auth].usage_file`, budgets reset when the router restarts.

## Streaming

Server-Sent Events (`text/event-stream`). Each token chunk is one
`data: {...}\n\n` line followed by `data: [DONE]\n\n`. The shape
matches OpenAI exactly:

```json
data: {"id":"chatcmpl-…","object":"chat.completion.chunk","created":1714566600,"model":"llama3-8b","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}
data: {"id":"chatcmpl-…","object":"chat.completion.chunk","created":1714566600,"model":"llama3-8b","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}
data: [DONE]
```

Failures before the first token are normal HTTP errors, never a 200 with an
empty stream. A failure after that sends one error event and ends without
`[DONE]`, so SDKs raise instead of treating a truncated answer as complete:

```json
data: {"error":{"message":"<text>","type":"server_error","code":"Internal"}}
```

The router never buffers a streaming response; tokens flow directly
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
| 403  | `permission_error` | valid token, but lacking the scope or model              |
| 404  | `model_not_found`  | unknown model name                                       |
| 400  | `invalid_request_error` | bad request, including one the engine rejected      |
| 413  | (none)             | body over `[router].max_body_mb`                         |
| 429  | (none)             | `cgn-ratelimit` request quota exhausted                  |
| 429  | `insufficient_quota` | a key's `tpm` or `budget` limit                        |
| 429  | `server_error`     | admission queue full or wait timed out                   |
| 503  | `server_error`     | no live node serving the model for the requested role    |
| 5xx  | `server_error`     | upstream failure (engine crash, transport)               |

## OpenAPI

A machine-readable spec (`openapi.yaml`) lives next to this doc and is
regenerated from `cgn-router::gateway::types` on every release. Use it
with `openapi-generator-cli` to scaffold typed clients in any
language.
