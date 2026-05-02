# llama3-8b · sglang · aggregated · HiCache

Single-GPU SGLang with [HiCache](https://docs.sglang.ai/advanced_features/hicache_design.html)
— SGLang's hierarchical KV cache — extending RadixAttention across GPU
HBM and host memory, optionally backed by NIXL/Mooncake.

This is the SGLang-side analogue of `vllm/agg-lmcache`.

## What gets injected

`[engine].kv_offload = "hicache"` makes `cgn-agent` append:

```
--enable-hierarchical-cache
--hicache-ratio 2
--hicache-write-policy write_through
--hicache-storage-backend nixl
```

to the `python -m sglang.launch_server` argv. To switch to a Mooncake
external pool, override `--hicache-storage-backend` and add
`--hicache-storage-backend-extra-config '{...}'` via
`[engine.sglang].extra_args`.

## GPU requirements

* 1 × GPU with ≥ 16 GiB HBM. HiCache adds host-RAM tier — the
  effective KV capacity is roughly `hicache_ratio` × GPU KV pool. With
  `hicache-ratio = 2`, plan for ~32 GiB host RAM headroom.

## Quick start

```bash
pip install "sglang[all]"
bash recipes/llama3-8b/sglang/agg-hicache/up.sh
```

Or:

```bash
cgn-ctl recipe up llama3-8b/sglang/agg-hicache
```

## Tear down

```bash
bash scripts/run/down.sh
```
