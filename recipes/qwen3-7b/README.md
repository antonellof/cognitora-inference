# Qwen-3 7B recipes

Bring-up profiles for `Qwen/Qwen3-7B-Instruct`.

| Topology                  | GPUs | Engine | Notes                                |
| ------------------------- | ---- | ------ | ------------------------------------ |
| [vllm/agg](vllm/agg)      | 1    | vLLM   | Single-node, TP=1, baseline          |
| [sglang/agg](sglang/agg)  | 1    | SGLang | RadixAttention prefix cache enabled  |

## Bring up

```bash
bash recipes/qwen3-7b/vllm/agg/up.sh
# or:
bash recipes/qwen3-7b/sglang/agg/up.sh
```

## Notes

- Qwen-3 is permissively licensed and does **not** require a HF token.
- Both recipes target a single 24 GiB GPU; they fit on consumer
  hardware (RTX 4090 / L4 / A10G).
