"""Wire-up tests for BackendAPI — verifies refresh / list / delete / copy
through mocked sub-clients (no real HTTP).
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

_PKG = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_PKG))

from backend.config import (  # noqa: E402
    Config, Pan123Config, R2Config, UpstashKvConfig,
)
from backend.index import IndexDB  # noqa: E402
from backend import orchestrator as orchestrator_module  # noqa: E402
from backend.orchestrator import BackendAPI  # noqa: E402


class FakeNiyen:
    def __init__(self, items):
        self.items = items
        self.calls = 0

    def list_feedback(self, *, since=None, limit=500):
        self.calls += 1
        return list(self.items)


class FakeR2:
    def __init__(self):
        self.deleted = []
        self.downloaded = []

    def download(self, key, target_path):
        self.downloaded.append((key, str(target_path)))
        target_path.parent.mkdir(parents=True, exist_ok=True)
        # Write a tiny zip file as a placeholder.
        import zipfile
        with zipfile.ZipFile(target_path, "w") as zf:
            zf.writestr("manifest.json", json.dumps({
                "summary": "from manifest",
                "email": "m@example.com",
                "app_version": "1.6.3",
                "os": "Windows 11",
                "gpu": "NVIDIA",
            }))
            zf.writestr("logs/current-session.log", "log line\n" * 5)
            zf.writestr("logs/incidents.log", "WARN something\n")

    def delete(self, key):
        self.deleted.append(key)

    def head(self, key):
        return None


class FakePan123:
    def __init__(self):
        self.deleted = []

    def download(self, bucket_path, target_path):
        # Reuse the same writer as FakeR2 for shape-compat.
        FakeR2().download(bucket_path, target_path)

    def delete(self, bucket_path):
        self.deleted.append(bucket_path)


class FakeKv:
    def __init__(self):
        self.deletes = []
        self.lrems = []

    def delete(self, key):
        self.deletes.append(key)
        return 1

    def lrem(self, list_key, value, *, count=0):
        self.lrems.append((list_key, value, count))
        return 1


def _build_config(tmp_path: Path) -> Config:
    cfg = Config(
        niyien_api_base="https://example.test/api",
        feedback_admin_token="t",
        pan123=Pan123Config(client_id="x", client_secret="y"),
        r2=R2Config(account_id="a", access_key_id="k", secret_access_key="s", bucket="b"),
        upstash_kv=UpstashKvConfig(url="https://kv.test", token="kt"),
        local_cache_dir=str(tmp_path / "cache" / "feedback"),
    )
    cfg.cache_root = Path(cfg.local_cache_dir)
    cfg.cache_root.mkdir(parents=True, exist_ok=True)
    cfg.config_path = tmp_path / "log_center.config.json"
    return cfg


def _build_backend(tmp_path: Path, niyien_items: list[dict] | None = None) -> tuple[BackendAPI, dict[str, Any]]:
    cfg = _build_config(tmp_path)
    db = IndexDB(tmp_path / "index.sqlite")
    fakes = {
        "niyien": FakeNiyen(niyien_items or []),
        "r2": FakeR2(),
        "pan123": FakePan123(),
        "kv": FakeKv(),
    }
    backend = BackendAPI(
        cfg, index_db=db,
        niyien=fakes["niyien"], r2=fakes["r2"],
        pan123=fakes["pan123"], kv=fakes["kv"],
    )
    return backend, fakes


def test_refresh_upserts(tmp_path):
    items = [
        {
            "id": "20260502-glo",
            "region": "global",
            "ts": "2026-05-02T12:00:00Z",
            "app_version": "1.6.3", "os": "Win", "gpu": "NV",
            "summary": "hello", "email": "", "size": 100,
            "bucket_path": "feedback/20260502/glo.zip",
        }
    ]
    backend, _ = _build_backend(tmp_path, items)
    res = backend.refresh()
    assert res["ok"]
    assert res["data"]["inserted"] == 1
    listed = backend.list({})
    assert listed["ok"]
    assert len(listed["data"]) == 1
    assert listed["data"][0]["id"] == "20260502-glo"


def test_download_and_open(tmp_path):
    items = [{
        "id": "20260502-glo", "region": "global",
        "ts": "2026-05-02T12:00:00Z",
        "size": 100, "bucket_path": "feedback/20260502/glo.zip",
    }]
    backend, fakes = _build_backend(tmp_path, items)
    backend.refresh()
    res = backend.download_one("20260502-glo")
    assert res["ok"], res
    assert fakes["r2"].downloaded
    extracted = Path(res["data"]["download_path"])
    assert (extracted / "manifest.json").exists()


def test_delete_one_routes_per_region(tmp_path):
    items = [
        {"id": "a", "region": "global", "ts": "2026-05-02T01:00:00Z",
         "size": 1, "bucket_path": "feedback/20260502/a.zip"},
        {"id": "20260502-cnx", "region": "cn", "ts": "2026-05-02T02:00:00Z",
         "size": 1, "bucket_path": "feedback/20260502/cnx.zip"},
    ]
    backend, fakes = _build_backend(tmp_path, items)
    backend.refresh()
    res1 = backend.delete_one("a")
    res2 = backend.delete_one("20260502-cnx")
    assert res1["ok"] and res2["ok"]
    assert fakes["r2"].deleted == ["feedback/20260502/a.zip"]
    assert fakes["pan123"].deleted == ["feedback/20260502/cnx.zip"]
    # KV: each id triggers a DEL fb:<id>; for 20260502-cnx also LREM.
    assert "fb:a" in fakes["kv"].deletes
    assert "fb:20260502-cnx" in fakes["kv"].deletes
    assert any(lk[0] == "fb:index:20260502" and lk[1] == "20260502-cnx" for lk in fakes["kv"].lrems)


def test_copy_prompt_renders_directory_prompt(tmp_path, monkeypatch):
    items = [{
        "id": "20260502-glo", "region": "global",
        "ts": "2026-05-02T12:00:00Z",
        "size": 100, "bucket_path": "feedback/20260502/glo.zip",
        "summary": "row summary", "email": "row@example.com",
    }]
    copied = {}

    def fake_clipboard_set(text, *, fallback_dir=None):
        copied["text"] = text
        return "test"

    monkeypatch.setattr(orchestrator_module, "clipboard_set", fake_clipboard_set)

    backend, _ = _build_backend(tmp_path, items)
    backend.refresh()
    download = backend.download_one("20260502-glo")
    assert download["ok"], download
    extracted = Path(download["data"]["download_path"])
    res = backend.copy_prompt("20260502-glo")
    assert res["ok"], res
    assert res["data"]["mechanism"] == "test"
    text = copied["text"]

    assert str(extracted) in text
    assert "from manifest" in text
    assert "1.6.3" in text
    assert "Windows 11" in text
    assert "NVIDIA" in text

    assert "row@example.com" not in text
    assert "m@example.com" not in text
    assert "log line" not in text
    assert "WARN something" not in text
    assert "Tail of the current session log" not in text
    assert "incidents.log" not in text
    assert ".gyroflow project" not in text
