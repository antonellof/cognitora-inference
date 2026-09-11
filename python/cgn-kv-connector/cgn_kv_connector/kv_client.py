"""Thin gRPC client for the host-local cgn-kvcached daemon."""

from __future__ import annotations

import os
from typing import Iterable

import grpc

from cognitora.v1 import kv_pb2, kv_pb2_grpc


def _endpoint() -> str:
    return os.environ.get("CGN_KVCACHED_GRPC", "127.0.0.1:7090")


def _channel() -> grpc.Channel:
    uds = os.environ.get("CGN_KVCACHED_UDS")
    if uds:
        return grpc.insecure_channel(f"unix://{uds}")
    return grpc.insecure_channel(_endpoint())


class KvCachedClient:
    """Blocking client for cgn-kvcached PutBlock / BatchLookup."""

    def __init__(self) -> None:
        self._stub = kv_pb2_grpc.KvStub(_channel())

    def put_block(
        self, prefix_hash: bytes, payload: bytes, model: str, layer: int = 0
    ) -> bool:
        if len(prefix_hash) != 32:
            raise ValueError("prefix_hash must be 32 bytes")
        resp = self._stub.PutBlock(
            kv_pb2.PutBlockSpec(
                prefix_hash=prefix_hash,
                payload=payload,
                model=model,
                layer=layer,
            )
        )
        return resp.code == 0

    def batch_lookup(self, digests: Iterable[bytes]) -> dict[bytes, int]:
        """Return {digest: size_bytes} for resident blocks."""
        values = [d for d in digests if len(d) == 32]
        if not values:
            return {}
        resp = self._stub.BatchLookup(kv_pb2.HashList(values=values))
        out: dict[bytes, int] = {}
        for entry in resp.entries:
            if entry.size_bytes > 0 and entry.prefix_hash:
                out[bytes(entry.prefix_hash)] = entry.size_bytes
        return out
