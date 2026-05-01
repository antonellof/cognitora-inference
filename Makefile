# Cognitora — top-level Makefile (all-Rust)
#
# Repository layout (see docs/architecture/repo-layout.md):
#   proto/                  gRPC source of truth (compiled by tonic-build at workspace build time)
#   rust/services/          Binary crates: cgn-router cgn-agent cgn-kvcached
#                                          cgn-metrics cgn-ctl cgn-operator
#   rust/libraries/         Library crates: cgn-proto cgn-core cgn-tls cgn-telemetry cgn-kv
#                                           cgn-auth cgn-ratelimit cgn-k8s cgn-helm cgn-power
#   deploy/                 Deployment artefacts (helm, systemd, terraform, docker, installer)

SHELL          := /usr/bin/env bash
ROOT           := $(shell pwd)
VERSION        ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo "v0.1.0-dev")
RUST_PROFILE   ?= release
CARGO_FLAGS    ?=
DOCKER_REPO    ?= ghcr.io/cognitora
PLATFORMS      ?= linux/amd64,linux/arm64

BINS := cgn-router cgn-agent cgn-kvcached cgn-metrics cgn-ctl cgn-operator

.PHONY: all help proto-lint proto-breaking \
        build test lint bench fmt fmt-check \
        docker helm-package install-tools clean

all: build ## Build everything

help: ## Show this help
	@awk 'BEGIN{FS=":.*##"} /^[a-zA-Z0-9_.-]+:.*##/ {printf "  \033[36m%-18s\033[0m %s\n",$$1,$$2}' $(MAKEFILE_LIST)

# ---------------------------------------------------------------------------
# Proto (tonic-build runs in cgn-proto/build.rs at compile time;
#        these targets just lint the .proto sources)
# ---------------------------------------------------------------------------

proto-lint: ## Lint proto files with buf
	buf lint

proto-breaking: ## Detect breaking proto changes against main
	buf breaking --against '.git#branch=main'

# ---------------------------------------------------------------------------
# Rust workspace
# ---------------------------------------------------------------------------

build: ## Build all crates (release by default; override with RUST_PROFILE=debug)
ifeq ($(RUST_PROFILE),release)
	cargo build --workspace --release $(CARGO_FLAGS)
else
	cargo build --workspace $(CARGO_FLAGS)
endif

test: ## Run unit + integration tests
	cargo test --workspace --all-features

lint: fmt-check ## Static checks (fmt + clippy)
	cargo clippy --workspace --all-features --all-targets -- -D warnings

fmt: ## Apply rustfmt
	cargo fmt --all

fmt-check: ## Verify formatting
	cargo fmt --all -- --check

bench: ## Run criterion benches
	cargo bench --workspace

install-tools: ## Install dev tools (buf, cargo-deny, etc.)
	@command -v buf >/dev/null    || (echo "install buf: https://buf.build/docs/installation"; exit 1)
	@command -v cosign >/dev/null || (echo "install cosign: https://docs.sigstore.dev/cosign/installation"; exit 1)
	cargo install --locked cargo-deny cargo-nextest sccache 2>/dev/null || true

# ---------------------------------------------------------------------------
# Docker / Helm
# ---------------------------------------------------------------------------

docker: ## Build all container images (multi-arch, distroless)
	@for b in $(BINS); do \
		echo "+ building docker image $$b"; \
		docker buildx build \
			--platform $(PLATFORMS) \
			--build-arg VERSION=$(VERSION) \
			--build-arg BIN=$$b \
			-t $(DOCKER_REPO)/$$b:$(VERSION) \
			-f deploy/docker/Dockerfile . ; \
	done

helm-package: ## Package the Helm chart
	@mkdir -p dist/
	helm package deploy/kubernetes/helm/cognitora -d dist/

# ---------------------------------------------------------------------------
# Cleaning
# ---------------------------------------------------------------------------

clean:
	cargo clean
	rm -rf bin/ dist/
