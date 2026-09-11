#!/usr/bin/env python3
"""Unit tests that run without vLLM or GPU."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from cgn_kv_connector.digests import request_prefix_digests  # noqa: E402


class PrefixDigestParsing(unittest.TestCase):
    def test_bytes_and_hex(self) -> None:
        raw = bytes(range(32))
        req = type("R", (), {"kv_transfer_params": {"cgn_prefix_digests": [raw.hex()]}})()
        out = request_prefix_digests(req)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0], raw)

    def test_empty(self) -> None:
        req = type("R", (), {"kv_transfer_params": {}})()
        self.assertEqual(request_prefix_digests(req), [])


if __name__ == "__main__":
    unittest.main()
