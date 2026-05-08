# cgn-agent

[![crates.io](https://img.shields.io/crates/v/cgn-agent.svg)](https://crates.io/crates/cgn-agent)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Per-node engine supervisor for a [Cognitora](https://github.com/antonellof/cognitora-inference)
cluster.

Responsibilities:

* Spawn and supervise the inference engine — vLLM, SGLang, llama.cpp, or
  any OpenAI-compatible server (TRT-LLM via thin driver).
* Translate `Agent.Generate` gRPC into engine HTTP calls.
* Report `NodeHealth` (NVML + queue depth + engine readiness) back to
  the router via etcd.
* Coordinate KV handoff with the colocated `cgn-kvcached` over UDS.
* Auto-render the right `--kv-transfer-config` JSON or
  `--enable-hierarchical-cache` flags for the configured KV offload
  backend (`none / nixl / lmcache / hicache / kvbm`).

## Install

```bash
curl -fsSL https://inference.cognitora.dev/install | bash
```

Or just this binary:

```bash
cargo install cgn-agent
```

## Run

```bash
cgn-agent --config /etc/cognitora/cognitora.toml
```

The engine binary itself (`vllm serve`, `sglang.launch_server`, etc.) is
spawned as a child process; install it independently per its own docs.

See [`docs/reference/config.md`](https://github.com/antonellof/cognitora-inference/blob/main/docs/reference/config.md).

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).
