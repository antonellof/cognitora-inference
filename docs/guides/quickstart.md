# Quickstart — 5 minutes from zero

This page walks from "I just heard about Cognitora" to "I'm
streaming tokens through it" without committing to a Kubernetes
cluster. We'll run **everything on one host**, with mTLS off and a
fake engine that returns canned tokens — perfect for a demo, a
laptop test, or a CI smoke check.

## Prerequisites

- Linux or macOS (Apple Silicon works; we'll use `--no-default-features`
  to skip rocksdb).
- Rust 1.89+ (or use the prebuilt installer below).
- ~2 minutes to build.

## 1. Install

Pick one of:

```bash
# Option A — prebuilt (Linux x86_64/aarch64, macOS arm64/x86_64)
curl -sSfL https://get.cognitora.dev | sh

# Option B — from source
git clone https://github.com/antonellof/open-inference cognitora
cd cognitora
cargo build --release --workspace --no-default-features \
    --exclude cgn-kvcached --exclude cgn-kv
```

If you used option B, the binaries are under `target/release/`.
Either prepend that to your `PATH` or use the full path below.

## 2. Bootstrap dev PKI

```bash
cgn-ctl pki bootstrap --out /tmp/pki --san localhost
```

You'll get four PEM files in `/tmp/pki`. We won't enable mTLS for
this run — but the files prove `cgn-ctl pki` works.

## 3. Issue an API key

```bash
mkdir -p /tmp/cognitora
cgn-ctl key create --file /tmp/cognitora/api-keys --scopes "chat,embed"
# prints: cgn-c782d73a8c914c3da49191626f95737e
export CGN_KEY=cgn-c782...   # paste the printed token
```

## 4. Minimal config

```bash
cat > /tmp/cognitora/cognitora.toml <<'EOF'
[cluster]
name     = "demo"
data_dir = "/tmp/cognitora"
etcd     = []                      # single-node, no etcd

[security]
require_mtls = false

[auth]
enabled       = true
api_keys_file = "/tmp/cognitora/api-keys"

[router]
listen_http  = "127.0.0.1:8080"
listen_grpc  = "127.0.0.1:7070"
listen_admin = "127.0.0.1:9091"

[models.demo]
hf_repo = "fake://demo"            # fake engine for the smoke test
EOF
```

## 5. Boot the router

```bash
cgn-router --config /tmp/cognitora/cognitora.toml &
sleep 2
```

You'll see a JSON log line per listener (HTTP on 8080, gRPC on 7070,
admin on 9091).

## 6. Hit it with the OpenAI SDK

```bash
curl -sSfL http://127.0.0.1:8080/v1/models \
  -H "authorization: bearer $CGN_KEY"
# {"object":"list","data":[{"id":"demo","object":"model","owned_by":"cognitora"}]}
```

A streaming chat completion (returns 503 until you wire up an agent
+ engine):

```bash
curl -N -sS http://127.0.0.1:8080/v1/chat/completions \
  -H "authorization: bearer $CGN_KEY" \
  -H "content-type: application/json" \
  -d '{
    "model": "demo",
    "messages": [{"role":"user","content":"Hello"}],
    "stream": true
  }'
```

## 7. What's next

You've now seen the gateway, the auth layer, and the routing
contract. To go further:

- **Add a real engine** → [bare-metal guide](baremetal.md) or
  [Kubernetes guide](kubernetes.md).
- **Understand the route** → [routing deep dive](../architecture/routing.md).
- **Cluster of one becomes cluster of N** → drop the
  `etcd = []` line, point at a real etcd, and start more agents.
- **Skim the OpenAI surface** → [API reference](../api/openai.md).

## 8. Tear down

```bash
kill %1                                # the backgrounded router
rm -rf /tmp/cognitora /tmp/pki
```
