"""Prefix digest parsing (no gRPC dependency)."""

from __future__ import annotations

from typing import Any


def request_prefix_digests(request: Any) -> list[bytes]:
    """Extract BLAKE3 digests attached by the Cognitora router, if any."""
    extra = getattr(request, "kv_transfer_params", None) or {}
    raw = extra.get("cgn_prefix_digests") or extra.get("prefix_hashes") or []
    out: list[bytes] = []
    for item in raw:
        if isinstance(item, (bytes, bytearray)) and len(item) == 32:
            out.append(bytes(item))
        elif isinstance(item, str) and len(item) == 64:
            out.append(bytes.fromhex(item))
    return out
