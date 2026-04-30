# AI Inference Platform -- Production Architecture (vLLM Distributed)

## Overview

This document defines a production-grade distributed AI inference
architecture using vLLM, optimized for efficiency, scalability, and
real-world infrastructure constraints.

------------------------------------------------------------------------

## Core Concepts

-   Separate Prefill and Decode workloads
-   KV Cache as a first-class resource
-   KV-aware routing (critical)
-   Model cascade (SLM → Mid → LLM)
-   Optimize for tokens/joule, not GPUs

------------------------------------------------------------------------

## Distributed vLLM Model

### Key Idea

vLLM is NOT a distributed system by itself.

Instead: - Scale OUT via replicas - Scale UP via tensor parallelism -
Use external orchestrator for intelligence

------------------------------------------------------------------------

## Architecture

                     ┌───────────────┐
                     │ API Gateway   │
                     └──────┬────────┘
                            │
                     ┌──────▼────────┐
                     │ Orchestrator  │
                     └──────┬────────┘
            ┌───────────────┼───────────────┐
            ▼               ▼               ▼
       vLLM Node A     vLLM Node B     vLLM Node C
            │               │               │
            └──── KV locality routing ─────┘

------------------------------------------------------------------------

## KV Cache Architecture

### Problem

KV cache is local to each node.

### Solution

-   Sticky routing
-   Prefix hashing
-   KV reuse

### KV Tiering

-   GPU (hot)
-   RAM (warm)
-   SSD (cold)

------------------------------------------------------------------------

## Routing Logic

``` python
def route(req):
    if kv_hit(req.prefix):
        return same_node
    elif req.long_context:
        return high_memory_node
    else:
        return least_loaded
```

------------------------------------------------------------------------

## Prefill / Decode Separation

### Prefill Cluster

-   High-end GPUs
-   Large batch
-   Builds KV

### Decode Cluster

-   Efficient GPUs / CPUs
-   Continuous batching

------------------------------------------------------------------------

## vLLM Configuration

### Prefill

    --tensor-parallel-size 4
    --gpu-memory-utilization 0.9
    --max-model-len 131072
    --enable-chunked-prefill

### Decode

    --enable-speculative-decoding
    --draft-model small-model

------------------------------------------------------------------------

## Kubernetes Deployment Example

``` yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: vllm-decode
spec:
  replicas: 5
  template:
    spec:
      containers:
      - name: vllm
        image: vllm/vllm:latest
```

------------------------------------------------------------------------

## Scheduling

### Power-aware scheduling

``` python
if rack_power > limit:
    reduce_precision()
    reroute()
```

### KV locality

``` python
if kv_score > 0.7:
    route_to_hot_node()
```

------------------------------------------------------------------------

## Metrics

-   TTFT
-   Tokens/sec
-   Tokens/joule
-   p95 latency
-   Cache hit rate

------------------------------------------------------------------------

## Anti-patterns

-   Round-robin routing
-   Single giant model
-   No KV reuse
-   Ignoring power constraints

------------------------------------------------------------------------

## Final Insight

The system scales not by adding GPUs, but by:

-   improving routing
-   reusing computation
-   optimizing energy
