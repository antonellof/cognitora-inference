# Security model

This page describes the trust boundaries, key material,
and the auth flow for the OpenAI HTTP surface.

## Trust boundaries

```
┌── Client (OpenAI SDK) ──┐
│  Authorization: Bearer  │  HTTPS
└────────────┬────────────┘
             │
┌────────────▼────────────┐
│   cgn-router :8080      │  ← public ingress, OIDC + API key
│   cgn-router :7070 mTLS │  ← internal mesh, peers below
└────┬────────────────┬───┘
     │ mTLS gRPC      │ mTLS gRPC
     ▼                ▼
┌──────────┐   ┌──────────────┐
│ cgn-agent│   │ cgn-kvcached │
└──────────┘   └──────────────┘
```

* **Public** surface (HTTP): any OpenAI client. Authenticated via API
  key (`Authorization: Bearer <key>`) or OIDC bearer token.
* **Internal** surface (gRPC + QUIC): only Cognitora processes,
  authenticated by mTLS leaf certificates rooted at a cluster CA when
  `[security].require_mtls = true`. The binary default is off, for local
  use. Production deployments must turn it on; gRPC has no other
  authentication.
* **KV peer traffic** (QUIC) carries data derived from prompts. With mTLS it
  requires a cluster-CA certificate holding the `[kv].quic_server_name` SAN.
  Without mTLS, `cgn-kvcached` refuses a non-loopback listener unless
  `[kv].allow_insecure_quic = true`.
* **Admin** surface (HTTP `:9091`/`:9092`): bound to `127.0.0.1` by
  default. The Helm chart never exposes these as Services.

## Tenancy

The tenant comes from the credential (`tenant:<id>` scope, else the key or
subject), never from a request field. Prefix digests are salted per tenant,
and vLLM receives a matching `cache_salt`, so one tenant can't probe another's
prompts through cache timing. Other engines rely on the router's routing
separation only: they keep a shared in-process cache.

The router's gRPC surface takes `tenant` from the caller and relies on mTLS.
It has no API-key auth, rate limits or token budgets, so expose it only to
trusted services.

## PKI

Two paths:

* `cgn-ctl pki bootstrap` generates a dev CA + leaf cert with rcgen
  and writes them under `/etc/cognitora/pki/`. Suitable for dev /
  single-node, **not for production**.
* External CA: set `[security] ca_file = ...` and provide leaf
  certs via your usual issuer (cert-manager, HashiCorp Vault, ACM PCA).

The internal CA must include all hostnames that other Cognitora nodes
will dial; in K8s the operator generates SANs for each Service.

## API auth

`cgn-auth::middleware::auth_middleware` runs ahead of every `/v1/*`
route. Authentication path:

1. If the request carries `Authorization: Bearer <token>`:
   - If the token's sha256 matches an entry in `[auth].api_keys_file`,
     the request proceeds as `apikey:<hash prefix>`.
   - Otherwise it is treated as a JWT and validated against the
     configured OIDC issuer's JWKS. The `sub` claim becomes the subject.
     An unknown key id triggers a JWKS refresh, at most every 30 s.
2. The middleware attaches the `Principal` as a request extension, where
   `cgn-ratelimit` and the handlers read it. Nothing is carried in a
   header, so clients can't forge an identity.
3. If `[auth].enabled = true` and no valid credential is present, the
   request gets `401`. If `[auth].enabled = false`, the middleware
   no-ops (dev / smoke tests only).
4. Each handler checks scopes, model access and token limits, returning
   `403` or `429`. See [scopes](../api/openai.md#scopes).

Rate limits key on the principal, else the client IP. `X-Forwarded-For`
counts only when the socket peer is in
`[router.rate_limit].trusted_proxies`, and then the right-most untrusted
hop is the client.

## Distroless

All production images are built `FROM gcr.io/distroless/cc-debian12:nonroot`.
There is no shell, no package manager, and the process runs as UID 65532.
The agent image is the only exception: it sits on top of the official
vLLM image because the engine wants Python + CUDA at runtime.

## Auditing

There is no audit log yet. Per-key spend is available through
`GET /v1/usage` and `cgn_router_tenant_tokens_total{tenant,kind}`, and the
router logs each routing decision at `info`, but neither records who sent
which request.
