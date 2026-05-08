# cgn-ctl

[![crates.io](https://img.shields.io/crates/v/cgn-ctl.svg)](https://crates.io/crates/cgn-ctl)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Admin CLI for [Cognitora](https://github.com/antonellof/cognitora-inference).

```text
cgn-ctl install   <target>          # bare-metal / k8s / cloud install
cgn-ctl cluster   <list|node|drain> # node operations
cgn-ctl model     <load|unload|ls>  # model orchestration
cgn-ctl recipe    <up|down|ls|show> # one-line bring-up of model recipes
cgn-ctl pki       <bootstrap|...>   # mTLS material
cgn-ctl key       <create|revoke>   # API keys
cgn-ctl bench     <chat|embed|...>  # micro-benchmarks
```

The CLI shells out to `helm` for Kubernetes installs (via
[`cgn-helm`](https://crates.io/crates/cgn-helm)) and renders CRDs from
[`cgn-k8s`](https://crates.io/crates/cgn-k8s). Pre-built release
tarballs ship a vendored `helm` binary so you don't need it on `$PATH`.

## Install

```bash
curl -fsSL https://inference.cognitora.dev/install | bash
```

Or:

```bash
cargo install cgn-ctl
```

## Examples

```bash
cgn-ctl install bare-metal
cgn-ctl recipe up vllm/qwen2.5-7b-instruct/single-host
cgn-ctl key create --scopes chat,embed alice
cgn-ctl bench chat --concurrency 32 --duration 60s
```

See [`docs/operations/`](https://github.com/antonellof/cognitora-inference/tree/main/docs/operations).

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).
