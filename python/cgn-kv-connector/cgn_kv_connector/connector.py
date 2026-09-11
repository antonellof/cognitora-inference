"""CognitoraConnector — vLLM KV offload into cgn-kvcached RAM/SSD tiers."""

from __future__ import annotations

import logging
import os
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Iterable

from cgn_kv_connector.kv_client import KvCachedClient

try:
    from vllm.distributed.kv_transfer.kv_connector.v1.base import KVConnectorBase_V1
except ImportError:  # pragma: no cover - importable without vLLM for packaging
    KVConnectorBase_V1 = object  # type: ignore[misc, assignment]

if TYPE_CHECKING:
    from vllm.forward_context import ForwardContext
    from vllm.v1.attention.backend import AttentionMetadata
    from vllm.v1.core.kv_cache_manager import KVCacheBlocks
    from vllm.v1.core.sched.output import SchedulerOutput
    from vllm.v1.request import Request

logger = logging.getLogger(__name__)


@dataclass
class CognitoraConnectorMetadata:
    """Scheduler → worker metadata for a step."""

    load_digests: list[bytes] = field(default_factory=list)
    save_digests: list[bytes] = field(default_factory=list)


class CognitoraConnector(KVConnectorBase_V1):
    """vLLM dynamic connector entry point (`cgn_kv_connector.connector`)."""

    def __init__(
        self,
        vllm_config: Any,
        role: Any,
        kv_cache_config: Any,
    ) -> None:
        super().__init__(vllm_config, role, kv_cache_config)
        self._client = KvCachedClient()
        self._model = vllm_config.model_config.model
        self._pending_saves: list[tuple[bytes, bytes, int]] = []
        self._connector_metadata: CognitoraConnectorMetadata | None = None

    def bind_connector_metadata(self, connector_metadata: Any) -> None:
        self._connector_metadata = connector_metadata

    def clear_connector_metadata(self) -> None:
        self._connector_metadata = None

    def start_load_kv(self, forward_context: "ForwardContext", **kwargs: Any) -> None:
        meta = self._connector_metadata
        if meta and meta.load_digests:
            logger.debug("cgn-kv: load %d digests", len(meta.load_digests))

    def wait_for_layer_load(self, layer_name: str) -> None:
        return

    def save_kv_layer(
        self,
        layer_name: str,
        kv_layer: Any,
        attn_metadata: "AttentionMetadata",
        **kwargs: Any,
    ) -> None:
        digest = kwargs.get("prefix_hash")
        if digest is None or len(digest) != 32:
            return
        try:
            payload = kv_layer.detach().cpu().numpy().tobytes()
        except Exception:
            payload = bytes(kv_layer)
        layer = int(kwargs.get("layer", 0))
        self._pending_saves.append((digest, payload, layer))

    def wait_for_save(self) -> None:
        for digest, payload, layer in self._pending_saves:
            if not self._client.put_block(digest, payload, self._model, layer):
                logger.warning("cgn-kv: PutBlock failed")
        self._pending_saves.clear()

    def get_num_new_matched_tokens(
        self, request: "Request", num_computed_tokens: int
    ) -> tuple[int | None, bool]:
        digests = _request_prefix_digests(request)
        if not digests:
            return 0, False
        resident = self._client.batch_lookup(digests)
        if not resident:
            return 0, False
        block_tokens = int(os.environ.get("CGN_KV_BLOCK_TOKENS", "16"))
        hits = sum(1 for d in digests if d in resident)
        return hits * block_tokens, False

    def update_state_after_alloc(
        self,
        request: "Request",
        blocks: "KVCacheBlocks",
        num_external_tokens: int,
    ) -> None:
        return

    def build_connector_meta(
        self, scheduler_output: "SchedulerOutput"
    ) -> CognitoraConnectorMetadata:
        return CognitoraConnectorMetadata()

    def request_finished(
        self,
        request: "Request",
        block_ids: list[int],
    ) -> tuple[bool, dict[str, Any] | None]:
        return False, None

    def take_events(self) -> Iterable[Any]:
        return []


def _request_prefix_digests(request: Any) -> list[bytes]:
    extra = getattr(request, "kv_transfer_params", None) or {}
    raw = extra.get("cgn_prefix_digests") or extra.get("prefix_hashes") or []
    out: list[bytes] = []
    for item in raw:
        if isinstance(item, (bytes, bytearray)) and len(item) == 32:
            out.append(bytes(item))
        elif isinstance(item, str) and len(item) == 64:
            out.append(bytes.fromhex(item))
    return out
