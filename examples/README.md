# Cognitora examples

Each subdirectory is a self-contained **profile** that
[`scripts/run/up.sh`](../scripts/run/up.sh) can boot directly. A profile
is just a folder of TOML files:

```
<profile>/
  router.toml         → cgn-router    (1, required)
  kvcached.toml       → cgn-kvcached  (0 or 1)
  agent-*.toml        → cgn-agent     (1..N, one per file)
  demo.sh             → optional smoke driver
  bench.sh            → optional benchmark driver
```

`up.sh` starts everything, registers the agents in etcd, and waits for
the router admin endpoint to be healthy. `down.sh <profile>` stops it
all in reverse order.

## Profiles in this folder

| Profile                                       | Boots via              | Engine                              | Best for                                    |
|-----------------------------------------------|------------------------|-------------------------------------|---------------------------------------------|
| [`local-mac/`](local-mac/README.md)           | `scripts/run/up.sh`    | `openai_compat` → Ollama            | macOS laptop. No Python venv, no GGUF download. |
| [`multi-llm/`](multi-llm/README.md)           | `scripts/run/up.sh`    | `vllm` (GPU) or `llama_cpp` (CPU)   | Linux box / server / CI. Multi-model with a real engine. |
| [`docker-ollama/`](docker-ollama/README.md)   | `docker compose up`    | `openai_compat` → Ollama            | Anywhere with Docker. Pulls the published GHCR image — no `cargo build`. |

All profiles share the same router, kvcached, and middleware code —
swapping topologies is *just* a matter of changing the `[engine]` block
inside each `agent-*.toml`.

## Running a binary profile

```bash
# Build the binaries once.
cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl

# Install a pinned local etcd (idempotent).
bash scripts/install/install-etcd.sh

# Bring up the profile (etcd → kvcached → agents → router).
bash scripts/run/up.sh examples/local-mac    # or examples/multi-llm

# Status / logs.
bash scripts/run/status.sh examples/local-mac

# Stop everything.
bash scripts/run/down.sh examples/local-mac
```

Each daemon writes its log to `~/.cache/cognitora/run/<name>.log` and
its pid to `<name>.pid`.

## Running the Docker Compose profile

```bash
cd examples/docker-ollama
docker compose up -d            # pulls ghcr.io/antonellof/cognitora
bash demo.sh                    # exercise the stack
docker compose down             # tear it all down
```

No build, no etcd install — `compose.yaml` ships its own etcd container
and pulls the published image. See
[`docker-ollama/README.md`](docker-ollama/README.md) for details.

## Smoke tests that do *not* need a profile

If you just want to verify the binaries built correctly without running
real models, see [`tests/`](../tests/README.md). The `multi_engine.sh`
script exercises the engine plugin layer + middleware in ~3 s using a
stub Python HTTP engine.

## Building a custom profile

Copy `multi-llm/` to a new folder, edit each `agent-*.toml` to point at
your engine, list models in `router.toml`, and run `up.sh`. There's no
project-level registration — the agents announce themselves in etcd at
startup and the router routes traffic to whoever's claimed the model
name.
