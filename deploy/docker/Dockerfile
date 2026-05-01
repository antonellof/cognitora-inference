# syntax=docker/dockerfile:1.7
# ---- Build stage -----------------------------------------------------------
FROM rust:1.89-slim-bookworm AS build

ARG BIN=cgn-router
ENV CARGO_TERM_COLOR=always

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev clang lld \
    cmake protobuf-compiler ca-certificates git \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# Linux build always enables the persistent (rocksdb) index.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p ${BIN} \
 && cp /src/target/release/${BIN} /usr/local/bin/${BIN}

# ---- Runtime: distroless --------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
ARG BIN=cgn-router
COPY --from=build /usr/local/bin/${BIN} /usr/local/bin/cognitora-bin
ENTRYPOINT ["/usr/local/bin/cognitora-bin"]
CMD ["--config", "/etc/cognitora/cognitora.toml"]
