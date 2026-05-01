# Runbooks

| Symptom                                  | Runbook                       |
|------------------------------------------|-------------------------------|
| Router returns 502/504 / hangs           | [router-down](router-down.md) |
| Cache hit ratio collapsed, TTFT spiking  | [cache-cold](cache-cold.md)   |
| Engine not ready / model 503 unavailable | [agent-stuck](agent-stuck.md) |

Every runbook follows the same structure:

1. **Symptoms** — the metric / log line / user complaint that brought
   you here.
2. **Triage** — numbered steps that branch based on what the signals
   show. Always start with the cheapest check.
3. **Prevention** — config changes / alerts that keep this from
   firing again.

Add a new runbook for any incident that took > 30 min to root-cause.
