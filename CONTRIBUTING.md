# Contributing to Cognitora

Thanks for your interest in contributing! Cognitora aims to be a
production-grade inference platform; the bar for incoming code is
high but the on-ramp should be quick.

## Quick start

```bash
git clone https://github.com/<org>/<repo> cognitora
cd cognitora
cargo build --release --workspace --no-default-features \
    --exclude cgn-kvcached --exclude cgn-kv
./tests/e2e/single_node.sh        # CPU smoke test
```

The exclusions skip rocksdb on dev hosts (it doesn't compile on
recent macOS SDKs). Linux production builds use the full feature set.

## Branching and review

- One change per PR. PRs that touch more than ~400 lines of
  non-trivial code will be asked to split.
- Branches are named `kebab-prefix/short-summary`, e.g.
  `router/cascade-confidence-floor`.
- Every PR runs the [`ci`](.github/workflows/ci.yml) workflow:
  `cargo fmt`, `cargo clippy --workspace --all-features
  -- -D warnings`, `cargo test`, `helm lint`, and `shellcheck` on
  the installer.

## Coding conventions

- **Errors**: `thiserror` for libraries, `anyhow` only at binary
  entry points. Never `unwrap()` outside tests.
- **Logging**: `tracing::{info,debug,warn,error}` with key=value
  fields. No `println!` in shipped code.
- **Async**: tokio everywhere. No blocking calls inside async
  functions — wrap with `spawn_blocking` if unavoidable.
- **Locks across awaits**: forbidden. `parking_lot::Mutex` guards
  must be dropped before any `.await`.
- **Unsafe**: only behind a named module, with a comment block
  explaining the invariant. The single approved location today is
  the `io_uring` plumbing in `cgn-kv`.
- **Public types**: `#[derive(Debug, Clone)]` by default, plus
  `serde::{Serialize, Deserialize}` for anything on the wire.

## Protobuf changes

- All wire types live in `proto/cognitora/v1/`.
- `buf lint` and `buf breaking --against '.git#branch=main'` run in
  CI; backwards-incompatible changes must use `reserved` and a new
  field number.
- Run `buf generate` locally before committing — the Rust stubs are
  checked in for fast clean builds.

## Tests

- Unit tests live next to the code (`#[cfg(test)] mod tests`).
- Integration tests under `tests/integration/<crate>/`.
- End-to-end smoke under `tests/e2e/` — start with
  [`single_node.sh`](tests/e2e/single_node.sh).
- Performance regression checks run from `tests/perf/`.

For any change that affects routing, admission, or KV cache
behaviour, add or update an integration test.

## Documentation

- Every new public function or wire type ships with `///`
  doc-comments and an example where the behaviour isn't obvious.
- High-level changes (new feature, new SLO, breaking change) update
  the matching deep-dive page under
  [`docs/architecture/`](docs/architecture/) or
  [`docs/operations/`](docs/operations/).
- The single source of truth for repository layout is
  [`docs/architecture/repo-layout.md`](docs/architecture/repo-layout.md);
  `README.md`'s layout section reflects it but is shorter.

## Commit messages

- Imperative mood, lowercase prefix scope:

  ```
  router: drop scoring tie when overlap == 0
  kv: gate rocksdb behind persistent-index feature
  docs: replace ASCII topology with a real SVG diagram
  ```

- Body explains the *why*; the diff already shows the *what*.
- Breaking changes carry a `BREAKING:` footer with the upgrade
  guidance.

## Releases

Maintainers tag `vX.Y.Z` on `main`. The
[`release`](.github/workflows/release.yml) workflow builds
multi-arch tarballs and container images, signs them with cosign,
and uploads to the GitHub Release page. See
[`SECURITY.md`](SECURITY.md) for verification.

## License

By contributing you agree your work is licensed under Apache-2.0
(the same license as the project).
