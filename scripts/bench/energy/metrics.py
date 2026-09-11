#!/usr/bin/env python3
"""Parse Cognitora Prometheus /metrics text for energy bench summaries."""
from __future__ import annotations

import re
import urllib.request


def fetch_metrics(url: str) -> str:
    with urllib.request.urlopen(url, timeout=30) as resp:
        return resp.read().decode("utf-8", errors="replace")


def sum_metric(text: str, name: str) -> float:
    total = 0.0
    pat = re.compile(rf"^{re.escape(name)}(?:\{{[^}}]*\}})?\s+([0-9.eE+-]+)", re.M)
    for m in pat.finditer(text):
        total += float(m.group(1))
    return total


def sample(url: str) -> dict[str, float]:
    text = fetch_metrics(url)
    return {
        "power_watts": sum_metric(text, "cgn_cluster_power_watts_total")
        or sum_metric(text, "cgn_cluster_node_power_watts"),
        "completion_tokens": sum_metric(text, "cgn_router_chat_completion_tokens_total"),
        "tokens_per_watt": sum_metric(text, "cgn_cluster_tokens_per_watt"),
    }
