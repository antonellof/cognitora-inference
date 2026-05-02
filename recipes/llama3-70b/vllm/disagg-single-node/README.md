# Llama-3.3-70B · vLLM · disaggregated · single-node

8×H100/H200 single-node deployment. Prefill runs on GPUs 0-3 (TP=4)
and decode runs on GPUs 4-7 (TP=4). KV blocks pass between the two
replicas through the colocated `cgn-kvcached` daemon over the
`NixlConnector`.

## GPU shape

| Resource | Required               |
| -------- | ---------------------- |
| GPUs     | 8× H100/H200 (80 GiB)  |
| Host RAM | 512 GiB+               |
| SSD      | 256 GiB cache dir      |

## Bring up

```bash
HF_TOKEN=<your-token> bash recipes/llama3-70b/vllm/disagg-single-node/up.sh
```

## Tear down

```bash
bash scripts/run/down.sh recipes/llama3-70b/vllm/disagg-single-node
```
