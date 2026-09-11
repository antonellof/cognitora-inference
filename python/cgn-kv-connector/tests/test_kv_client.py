#!/usr/bin/env python3
"""KvCachedClient validation tests (no live gRPC)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from cgn_kv_connector.digests import validate_prefix_hash  # noqa: E402


class ValidatePrefixHash(unittest.TestCase):
    def test_accepts_32_bytes(self) -> None:
        validate_prefix_hash(bytes(32))

    def test_rejects_short(self) -> None:
        with self.assertRaises(ValueError):
            validate_prefix_hash(b"short")


if __name__ == "__main__":
    unittest.main()
