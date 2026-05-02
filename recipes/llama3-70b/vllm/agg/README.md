# Llama-3.3-70B · vLLM · aggregated

Single-node aggregated, 4×H100/H200, TP=4, FP8 dynamic quantization.

## GPU shape

| Resource | Required               |
| -------- | ---------------------- |
| GPUs     | 4× H100/H200 (80 GiB)  |
| Host RAM | 256 GiB+               |
| SSD      | 256 GiB cache dir      |

## Bring up

```bash
HF_TOKEN=<your-token> bash recipes/llama3-70b/vllm/agg/up.sh
```

The first launch downloads `RedHatAI/Llama-3.3-70B-Instruct-FP8-dynamic`
from HuggingFace (~70 GiB).

## Tear down

```bash
bash scripts/run/down.sh recipes/llama3-70b/vllm/agg
```
