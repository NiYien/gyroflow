"""Smoke tests for backend.index.IndexDB — schema, upsert preservation,
filtering.
"""

from __future__ import annotations

import sys
from pathlib import Path

_PKG = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_PKG))

from backend.index import IndexDB  # noqa: E402


def _record(id_: str, *, region: str = "global", ts: str = "2026-05-02T12:00:00Z",
            bucket_path: str = "feedback/20260502/foo.zip", size: int = 1024) -> dict:
    return {
        "id": id_,
        "region": region,
        "ts": ts,
        "app_version": "1.6.3-niyien.1",
        "os": "Windows 11",
        "gpu": "NVIDIA",
        "summary": "test",
        "email": "u@example.com",
        "size": size,
        "bucket_path": bucket_path,
    }


def test_schema_created(tmp_path):
    db = IndexDB(tmp_path / "index.sqlite")
    rows = db.list()
    assert rows == []
    db.close()


def test_upsert_preserves_local_only_columns(tmp_path):
    db = IndexDB(tmp_path / "index.sqlite")
    rec = _record("20260502-aaa")
    db.upsert_records([rec])
    # Mark downloaded + add note locally.
    db.update_download("20260502-aaa", str(tmp_path / "out"))
    db.update_notes("20260502-aaa", "investigate later")

    # Re-upsert with updated server fields; local-only must survive.
    rec2 = _record("20260502-aaa", size=4096)
    rec2["summary"] = "changed by server"
    inserted, updated = db.upsert_records([rec2])
    assert (inserted, updated) == (0, 1)
    row = db.get("20260502-aaa")
    assert row["summary"] == "changed by server"
    assert row["size"] == 4096
    assert row["downloaded"] == 1
    assert row["download_path"] == str(tmp_path / "out")
    assert row["notes"] == "investigate later"
    db.close()


def test_filter_by_region(tmp_path):
    db = IndexDB(tmp_path / "index.sqlite")
    db.upsert_records([
        _record("20260502-cn1", region="cn"),
        _record("20260502-glo", region="global"),
    ])
    assert {r["id"] for r in db.list(region="cn")} == {"20260502-cn1"}
    assert {r["id"] for r in db.list(region="global")} == {"20260502-glo"}
    assert len(db.list()) == 2
    db.close()


def test_filter_by_downloaded(tmp_path):
    db = IndexDB(tmp_path / "index.sqlite")
    db.upsert_records([_record("a"), _record("b")])
    db.update_download("a", str(tmp_path / "a"))
    assert {r["id"] for r in db.list(downloaded=True)} == {"a"}
    assert {r["id"] for r in db.list(downloaded=False)} == {"b"}
    db.close()


def test_corrupt_db_recreated(tmp_path):
    p = tmp_path / "index.sqlite"
    p.write_bytes(b"this is not a sqlite file")
    # Should not raise; should move file aside and create empty db.
    db = IndexDB(p)
    assert db.list() == []
    db.close()
    # Backup file should exist.
    backups = list(tmp_path.glob("index.sqlite.bak.*"))
    assert backups, "expected a .bak.* backup of the corrupt db"
