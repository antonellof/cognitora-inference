# Llama-3.3 70B recipes

Production bring-up profiles for `meta-llama/Llama-3.3-70B-Instruct`.
Both recipes target NVIDIA H100/H200 class hardware and use FP8
weights for fit + perf.

| Topology                                            | GPUs | Notes                                                       |
| --------------------------------------------------- | ---- | ----------------------------------------------------------- |
| [vllm/agg](vllm/agg)                                | 4    | Single-node aggregated, TP=4                                |
| [vllm/disagg-single-node](vllm/disagg-single-node)  | 8    | TP=4 prefill on GPUs 0-3, TP=4 decode on GPUs 4-7           |

## Bring up

```bash
bash recipes/llama3-70b/vllm/agg/up.sh
# or:
bash recipes/llama3-70b/vllm/disagg-single-node/up.sh
```

## Notes

- The model is gated on HuggingFace; export `HF_TOKEN=...` before
  running `up.sh` so vLLM can pull weights on first launch.
- `cgn-kvcached` is sized for 70B (60 GiB RAM tier, 256 GiB SSD tier);
  adjust to your host before deploying.
- The disaggregated topology does not require a separate Cognitora
  install — the router and KV daemon are shared with the agents.
