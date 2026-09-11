# Disaggregation benchmark summary

Model: `test` · n=8 · concurrency=? · max_tokens=? · prompt_tokens=?

| mode | ok/n | TTFT p50 (ms) | TTFT p95 (ms) | decode tok/s (p50) | system tok/s |
|---|---:|---:|---:|---:|---:|
| disagg | 8/8 | 120 | 340 | 42.1 | 81.9 |
| agg | 8/8 | 180 | 420 | 38.0 | 72.5 |

## disagg vs agg (positive = disagg higher)

| metric | disagg | agg | delta |
|---|---:|---:|---:|
| TTFT p50 (ms) | 120 | 180 | -33.3% |
| TTFT p95 (ms) | 340 | 420 | -19.0% |
| system tok/s | 81.9 | 72.5 | +13.0% |
