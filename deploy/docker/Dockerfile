# syntax=docker/dockerfile:1.7
#
# Single-image build: ships all six Cognitora binaries (cgn-router,
# cgn-agent, cgn-kvcached, cgn-metrics, cgn-ctl, cgn-operator) in
# /usr/local/bin/. Pods choose which one to run via `command:`; the
# Helm chart already does this. `docker run` with no override boots
# the router by default.
#
# Why one image instead of six:
#   * compiles shared library crates (cgn-core, cgn-proto, cgn-tls,
#     cgn-kv, ...) exactly once via a single `cargo build` invocation,
#   * single docker job in CI instead of a 6-way matrix,
#   * one image to pull on every node — Kubernetes layer cache reuse
#     is automatic across all components.

# ---- Build stage -----------------------------------------------------------
FROM rust:1.89-slim-bookworm AS build

ENV CARGO_TERM_COLOR=always

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev clang lld \
    cmake protobuf-compiler libprotobuf-dev ca-certificates git \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# Single cargo invocation builds all six binaries with default
# features (rocksdb-backed persistent KV index enabled). Cache mounts
# survive across docker layers and `docker buildx --cache-to/--cache-from`
# carries them across CI runs.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release \
      -p cgn-router -p cgn-agent -p cgn-kvcached \
      -p cgn-metrics -p cgn-ctl -p cgn-operator \
 && mkdir -p /out \
 && cp /src/target/release/cgn-router \
       /src/target/release/cgn-agent \
       /src/target/release/cgn-kvcached \
       /src/target/release/cgn-metrics \
       /src/target/release/cgn-ctl \
       /src/target/release/cgn-operator \
       /out/

# ---- Runtime: distroless --------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=build /out/cgn-router    /usr/local/bin/cgn-router
COPY --from=build /out/cgn-agent     /usr/local/bin/cgn-agent
COPY --from=build /out/cgn-kvcached  /usr/local/bin/cgn-kvcached
COPY --from=build /out/cgn-metrics   /usr/local/bin/cgn-metrics
COPY --from=build /out/cgn-ctl       /usr/local/bin/cgn-ctl
COPY --from=build /out/cgn-operator  /usr/local/bin/cgn-operator

# `docker run` boots the router by default. Override with `command:`
# (Kubernetes) or `--entrypoint` (docker run) to launch a different
# binary, e.g. `--entrypoint /usr/local/bin/cgn-agent`.
ENTRYPOINT []
CMD ["/usr/local/bin/cgn-router", "--config", "/etc/cognitora/cognitora.toml"]
