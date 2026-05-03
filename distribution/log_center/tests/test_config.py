"""Smoke tests for backend.config — load_config valid / missing / malformed."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# Make the package importable when running pytest from any cwd.
_PKG = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_PKG))

from backend.config import ConfigError, load_config  # noqa: E402


def _good_payload() -> dict:
    return {
        "niyien_api_base": "https://niyien.com/api",
        "feedback_admin_token": "x" * 64,
        "pan123": {
            "client_id": "cid",
            "client_secret": "csec",
            "feedback_root_dir": "/feedback",
        },
        "r2": {
            "account_id": "acc",
            "access_key_id": "akid",
            "secret_access_key": "sk",
            "bucket": "gyroflow-feedback",
        },
        "upstash_kv": {
            "url": "https://example.upstash.io",
            "token": "kv-token",
        },
        "local_cache_dir": "_cache/feedback",
    }


def test_load_valid(tmp_path):
    p = tmp_path / "log_center.config.json"
    p.write_text(json.dumps(_good_payload()), encoding="utf-8")
    cfg = load_config(p)
    assert cfg.niyien_api_base == "https://niyien.com/api"
    assert cfg.r2.bucket == "gyroflow-feedback"
    assert cfg.cache_root.exists()


def test_missing_field(tmp_path):
    payload = _good_payload()
    del payload["feedback_admin_token"]
    p = tmp_path / "log_center.config.json"
    p.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(ConfigError) as exc:
        load_config(p)
    assert "feedback_admin_token" in str(exc.value)


def test_missing_section(tmp_path):
    payload = _good_payload()
    del payload["r2"]
    p = tmp_path / "log_center.config.json"
    p.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(ConfigError) as exc:
        load_config(p)
    assert "r2" in str(exc.value)


def test_malformed_json(tmp_path):
    p = tmp_path / "log_center.config.json"
    p.write_text("{not valid json", encoding="utf-8")
    with pytest.raises(ConfigError):
        load_config(p)


def test_missing_file(tmp_path):
    p = tmp_path / "missing.json"
    with pytest.raises(ConfigError) as exc:
        load_config(p)
    assert "Config file not found" in str(exc.value)
