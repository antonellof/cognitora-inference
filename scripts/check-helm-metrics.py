#!/usr/bin/env python3
"""Fail if the Helm rules or dashboard use a metric the code doesn't export.

Rules on a missing metric are valid PromQL that silently never fires, which is
how the chart's alerts stayed broken. Run from the repo root; needs `helm`.
"""
import re
import subprocess
import sys
from pathlib import Path

CHART = "deploy/kubernetes/helm/cognitora"
SUFFIXES = ("_bucket", "_sum", "_count")


def exported():
    names = set()
    for f in Path("rust").rglob("*.rs"):
        names.update(re.findall(r'"(cgn_[a-z0-9_]+)"', f.read_text()))
    return names


def rendered():
    out = subprocess.run(
        ["helm", "template", "check", CHART,
         "--set", "metrics.prometheusRule.enabled=true",
         "--set", "metrics.dashboards.enabled=true",
         "--set", "metrics.serviceMonitor.enabled=true"],
        check=True, capture_output=True, text=True).stdout
    return out


def base(name, known):
    if name in known:
        return name
    for s in SUFFIXES:
        if name.endswith(s) and name[: -len(s)] in known:
            return name[: -len(s)]
    return None


def main():
    known = exported()
    used = set(re.findall(r"\bcgn_[a-z0-9_]+", rendered()))
    # Config keys and the chart's own names aren't metrics.
    used = {u for u in used if u.startswith(("cgn_router_", "cgn_cluster_", "cgn_power_", "cgn_carbon_", "cgn_agent_", "cgn_kv"))}
    missing = sorted(u for u in used if base(u, known) is None)
    if not used:
        sys.exit("no metrics found in the rendered chart; the check is broken")
    if missing:
        sys.exit("metrics used by the chart but not exported:\n  " + "\n  ".join(missing))
    print(f"ok: {len(used)} metrics in the chart all exist in the code")


if __name__ == "__main__":
    main()
