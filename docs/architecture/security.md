# Security model

Cognitora is mTLS-by-default and hard to mis-configure into "open
internet" mode. This page describes the trust boundaries, key material,
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
  authenticated by mTLS leaf certificates rooted at a cluster CA.
* **Admin** surface (HTTP `:9091`/`:9092`): bound to `127.0.0.1` by
  default. The Helm chart never exposes these as Services.

## PKI

Two paths:

* `cgn-ctl pki bootstrap` — generates a dev CA + leaf cert with rcgen
  and writes them under `/etc/cognitora/pki/`. Suitable for dev /
  single-node, **not for production**.
* External CA — set `[security] ca_file = ...` and provide leaf
  certs via your usual issuer (cert-manager, HashiCorp Vault, ACM PCA).

The internal CA must include all hostnames that other Cognitora nodes
will dial; in K8s the operator generates SANs for each Service.

## API auth

`cgn-auth::middleware::auth_middleware` runs ahead of every `/v1/*`
route. Authentication path:

1. If the request carries `Authorization: Bearer <token>`:
   - If `<token>` matches the sha256 of an entry in `[auth].api_keys_file`,
     the request proceeds with `subject = "key:<id>"`.
   - Otherwise the token is treated as a JWT and validated against the
     configured OIDC issuer's JWKS. The `sub` claim becomes
     `subject = "oidc:<sub>"`.
2. The middleware sets the `x-cgn-subject` header on the inner
   request so `cgn-ratelimit` and the gateway handlers can read it
   without re-doing the lookup.
3. If no auth material is present and `[auth].enabled = true`, the
   request is rejected with `401`. If `[auth].enabled = false`, the
   middleware no-ops (dev / smoke tests only).

## Distroless

All production images are built `FROM gcr.io/distroless/cc-debian12:nonroot`.
There is no shell, no package manager, and the process runs as UID 65532.
The agent image is the only exception: it sits on top of the official
vLLM image because the engine wants Python + CUDA at runtime.

## Auditing

Every authenticated request emits:

* a `tracing` span with `subject`, `model`, and `request_id`,
* a Prometheus counter `cgn_router_requests_total{auth, model}`,
* an OTLP span when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
