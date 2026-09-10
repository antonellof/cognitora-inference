#!/usr/bin/env python3
"""Mock Prometheus /metrics endpoint for the Cognitora cluster dashboard.

Serves a CORS-enabled Prometheus text exposition that simulates a
16-node mixed GPU fleet (H100 / H200 / A100 / MI300X / L40S / A10)
serving five models under a slowly waving load: request/token counters,
latency + TTFT histograms, per-node queue depth, KV-cache pressure,
power draw against soft caps, prefix-index growth, and the occasional
node outage.

Zero dependencies — Python 3.8+ stdlib only.

Usage:

    python3 dashboard/mock_metrics.py [--port 9099]

then point the dashboard (dashboard/index.html) at:

    http://localhost:9099/metrics

Every series name and label matches what a real cgn-router admin
listener exposes, so the dashboard cannot tell the difference.
"""
from __future__ import annotations

import argparse
import math
import random
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# --------------------------------------------------------------------------
# Fleet topology
# --------------------------------------------------------------------------

# node_id, gpu, vendor, vram_mb, idle_w, cap_w, model, total_kv_blocks
FLEET = [
    ("h100-rack1-01", "NVIDIA H100 80GB HBM3", "nvidia", 81559, 290, 700, "llama3-70b", 40960),
    ("h100-rack1-02", "NVIDIA H100 80GB HBM3", "nvidia", 81559, 300, 700, "llama3-70b", 40960),
    ("h100-rack1-03", "NVIDIA H100 80GB HBM3", "nvidia", 81559, 285, 700, "llama3-70b", 40960),
    ("h100-rack1-04", "NVIDIA H100 80GB HBM3", "nvidia", 81559, 295, 700, "llama3-70b", 40960),
    ("h200-rack1-05", "NVIDIA H200 141GB HBM3e", "nvidia", 143771, 310, 700, "qwen3-235b", 73728),
    ("h200-rack1-06", "NVIDIA H200 141GB HBM3e", "nvidia", 143771, 305, 700, "qwen3-235b", 73728),
    ("a100-rack2-01", "NVIDIA A100-SXM4-80GB", "nvidia", 81920, 180, 400, "deepseek-r1-distill", 32768),
    ("a100-rack2-02", "NVIDIA A100-SXM4-80GB", "nvidia", 81920, 175, 400, "deepseek-r1-distill", 32768),
    ("a100-rack2-03", "NVIDIA A100-SXM4-80GB", "nvidia", 81920, 185, 400, "deepseek-r1-distill", 32768),
    ("a100-rack2-04", "NVIDIA A100-SXM4-80GB", "nvidia", 81920, 178, 400, "mixtral-8x7b", 32768),
    ("mi300x-rack3-01", "AMD Instinct MI300X", "amd", 196608, 320, 750, "qwen3-235b", 65536),
    ("mi300x-rack3-02", "AMD Instinct MI300X", "amd", 196608, 315, 750, "mixtral-8x7b", 65536),
    ("l40s-edge-01", "NVIDIA L40S", "nvidia", 46068, 95, 350, "phi-4", 16384),
    ("l40s-edge-02", "NVIDIA L40S", "nvidia", 46068, 92, 350, "phi-4", 16384),
    ("a10-edge-03", "NVIDIA A10", "nvidia", 24564, 55, 150, "phi-4", 8192),
    ("a10-edge-04", "NVIDIA A10", "nvidia", 24564, 58, 150, "phi-4", 8192),
]

# model → (base req/s at peak, mean completion tokens, latency scale s,
#          ttft scale s)
MODELS = {
    "llama3-70b": (14.0, 320, 2.2, 0.28),
    "qwen3-235b": (6.0, 410, 3.6, 0.42),
    "deepseek-r1-distill": (22.0, 240, 1.1, 0.14),
    "mixtral-8x7b": (18.0, 260, 0.9, 0.11),
    "phi-4": (34.0, 140, 0.45, 0.06),
}

LATENCY_LE = [0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 20, 60]
TTFT_LE = [0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10]


def load_wave(t: float, phase: float = 0.0) -> float:
    """0..1 load factor: an 8-minute primary wave + a 45-second ripple."""
    slow = 0.5 + 0.5 * math.sin(2 * math.pi * t / 480 + phase)
    fast = 0.08 * math.sin(2 * math.pi * t / 45 + phase * 2.7)
    return max(0.05, min(1.0, 0.25 + 0.65 * slow + fast))


class Hist:
    """Cumulative Prometheus histogram fed by sampled observations."""

    def __init__(self, edges):
        self.edges = edges
        self.buckets = [0] * (len(edges) + 1)  # last = +Inf
        self.count = 0
        self.total = 0.0

    def observe(self, v: float):
        self.count += 1
        self.total += v
        for i, e in enumerate(self.edges):
            if v <= e:
                self.buckets[i] += 1
                return
        self.buckets[-1] += 1

    def render(self, name: str, labels: str) -> list[str]:
        out, cum = [], 0
        sep = "," if labels else ""
        for e, b in zip(self.edges, self.buckets):
            cum += b
            le = format(e, "g")
            out.append(f'{name}_bucket{{{labels}{sep}le="{le}"}} {cum}')
        out.append(f'{name}_bucket{{{labels}{sep}le="+Inf"}} {self.count}')
        out.append(f"{name}_sum{{{labels}}} {self.total:.3f}")
        out.append(f"{name}_count{{{labels}}} {self.count}")
        return out


class Sim:
    """Advances the fleet simulation and renders the exposition."""

    def __init__(self):
        self.lock = threading.Lock()
        self.t0 = time.time()
        self.last = self.t0
        self.req = {m: {"200": 0.0, "5xx": 0.0} for m in MODELS}
        self.tokens = {m: 0.0 for m in MODELS}
        self.lat = {(m, s): Hist(LATENCY_LE) for m in MODELS for s in ("true", "false")}
        self.ttft = {m: Hist(TTFT_LE) for m in MODELS}
        self.prefix_digests = 128
        self.outage_node = None
        self.outage_until = 0.0
        # give the charts an instant backstory: pre-roll ~40 min
        for _ in range(480):
            self.last -= 5.0
        for _ in range(480):
            self._step(5.0)

    def _sample_latency(self, scale: float, load: float) -> float:
        # lognormal-ish; the tail fattens as the cluster loads up
        v = random.lognormvariate(math.log(scale), 0.45 + 0.5 * load)
        return min(v, 60.0)

    def _step(self, dt: float):
        t = self.last - self.t0
        self.last += dt

        # occasional 30–75 s single-node outage, roughly every 6 minutes
        now = self.last
        if self.outage_node and now > self.outage_until:
            self.outage_node = None
        if not self.outage_node and random.random() < dt / 360.0:
            self.outage_node = random.choice(FLEET)[0]
            self.outage_until = now + random.uniform(30, 75)

        for mi, (model, (base_rps, mean_toks, lat_s, ttft_s)) in enumerate(MODELS.items()):
            load = load_wave(t, phase=mi * 1.1)
            n = max(0, int(random.gauss(base_rps * load * dt, math.sqrt(dt))))
            errs = sum(1 for _ in range(n) if random.random() < 0.004)
            self.req[model]["200"] += n - errs
            self.req[model]["5xx"] += errs
            self.tokens[model] += (n - errs) * random.gauss(mean_toks, mean_toks * 0.2)
            # sample a capped number of observations per tick to stay cheap
            for _ in range(min(n, 40)):
                stream = "true" if random.random() < 0.6 else "false"
                self.lat[(model, stream)].observe(self._sample_latency(lat_s, load))
                self.ttft[model].observe(self._sample_latency(ttft_s, load) * 0.9)

        self.prefix_digests = min(
            200_000, self.prefix_digests + int(random.uniform(0, 6) * dt)
        )

    def advance(self):
        with self.lock:
            dt = time.time() - self.last
            while dt > 0:
                step = min(dt, 5.0)
                self._step(step)
                dt -= step

    def render(self) -> str:
        self.advance()
        with self.lock:
            t = self.last - self.t0
            L = []
            add = L.append

            add("# HELP cgn_router_chat_requests_total Chat completions served.")
            add("# TYPE cgn_router_chat_requests_total counter")
            for m, by in self.req.items():
                for status, v in by.items():
                    add(f'cgn_router_chat_requests_total{{model="{m}",status="{status}"}} {int(v)}')

            add("# TYPE cgn_router_chat_completion_tokens_total counter")
            for m, v in self.tokens.items():
                add(f'cgn_router_chat_completion_tokens_total{{model="{m}"}} {int(v)}')

            add("# TYPE cgn_router_chat_latency_seconds histogram")
            for (m, s), h in self.lat.items():
                L += h.render("cgn_router_chat_latency_seconds", f'model="{m}",stream="{s}"')
            add("# TYPE cgn_router_chat_ttft_seconds histogram")
            for m, h in self.ttft.items():
                L += h.render("cgn_router_chat_ttft_seconds", f'model="{m}"')

            add("# TYPE cgn_router_prefix_index_digests gauge")
            add(f"cgn_router_prefix_index_digests {self.prefix_digests}")
            add("# TYPE cgn_cluster_nodes_total gauge")
            add(f"cgn_cluster_nodes_total {len(FLEET)}")

            for i, (nid, gpu, vendor, vram, idle_w, cap_w, model, total) in enumerate(FLEET):
                load = load_wave(t, phase=i * 0.7)
                up = 0 if nid == self.outage_node else 1
                if not up:
                    load = 0.0
                queue = int(load * 16 + random.uniform(0, 2))
                free = int(total * (1.0 - 0.8 * load))
                watts = 0.0 if not up else round(
                    idle_w + (cap_w * 0.92 - idle_w) * load + random.uniform(-8, 8), 1
                )
                lbl = f'node="{nid}"'
                add(
                    f'cgn_cluster_node_info{{address="http://10.40.{i // 4}.{10 + i}:7070",'
                    f'gpu="{gpu}",gpu_vendor="{vendor}",model="{model}",{lbl},role="both"}} 1'
                )
                add(f"cgn_cluster_node_up{{{lbl}}} {up}")
                add(f"cgn_cluster_node_cordoned{{{lbl}}} 0")
                add(f"cgn_cluster_node_queue_depth{{{lbl}}} {queue}")
                add(f"cgn_cluster_node_kv_free_blocks{{{lbl}}} {free}")
                add(f"cgn_cluster_node_kv_total_blocks{{{lbl}}} {total}")
                add(f"cgn_cluster_node_power_watts{{{lbl}}} {watts}")
                add(f"cgn_cluster_node_watt_limit{{{lbl}}} {cap_w}")
                add(f"cgn_cluster_node_vram_total_mb{{{lbl}}} {vram}")

            return "\n".join(L) + "\n"


SIM = Sim()


class Handler(BaseHTTPRequestHandler):
    def _cors(self):
        self.send_header("access-control-allow-origin", "*")
        self.send_header("access-control-allow-methods", "GET, OPTIONS")
        self.send_header("access-control-allow-headers", "*")

    def do_OPTIONS(self):  # noqa: N802
        self.send_response(204)
        self._cors()
        self.end_headers()

    def do_GET(self):  # noqa: N802
        if self.path.split("?")[0] not in ("/metrics", "/"):
            self.send_response(404)
            self.end_headers()
            return
        body = SIM.render().encode()
        self.send_response(200)
        self._cors()
        self.send_header("content-type", "text/plain; version=0.0.4; charset=utf-8")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):  # keep the terminal quiet
        pass


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", type=int, default=9099)
    args = ap.parse_args()
    srv = ThreadingHTTPServer(("0.0.0.0", args.port), Handler)
    print(f"mock Cognitora metrics on http://localhost:{args.port}/metrics")
    print("point dashboard/index.html at that URL")
    srv.serve_forever()


if __name__ == "__main__":
    main()
