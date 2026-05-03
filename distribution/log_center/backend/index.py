"""SQLite-backed local index of feedback records.

Schema (see design D3 / spec):

    CREATE TABLE feedback (
      id TEXT PRIMARY KEY,
      region TEXT NOT NULL,
      ts TEXT NOT NULL,
      app_version TEXT,
      os TEXT,
      gpu TEXT,
      summary TEXT,
      email TEXT,
      size INTEGER,
      bucket_path TEXT NOT NULL,
      downloaded INTEGER NOT NULL DEFAULT 0,
      download_path TEXT,
      notes TEXT
    );
    CREATE INDEX idx_ts ON feedback(ts DESC);

`upsert_records` preserves `downloaded` / `download_path` / `notes` across
refreshes (server never owns those columns).
"""

from __future__ import annotations

import sqlite3
import time
from contextlib import contextmanager
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Any, Iterator, Optional

SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS feedback (
    id TEXT PRIMARY KEY,
    region TEXT NOT NULL,
    ts TEXT NOT NULL,
    app_version TEXT,
    os TEXT,
    gpu TEXT,
    summary TEXT,
    email TEXT,
    size INTEGER,
    bucket_path TEXT NOT NULL,
    downloaded INTEGER NOT NULL DEFAULT 0,
    download_path TEXT,
    notes TEXT
);
CREATE INDEX IF NOT EXISTS idx_ts ON feedback(ts DESC);
"""


@dataclass
class FeedbackRow:
    id: str
    region: str
    ts: str
    app_version: Optional[str] = None
    os: Optional[str] = None
    gpu: Optional[str] = None
    summary: Optional[str] = None
    email: Optional[str] = None
    size: Optional[int] = None
    bucket_path: str = ""
    downloaded: int = 0
    download_path: Optional[str] = None
    notes: Optional[str] = None

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class IndexDB:
    """sqlite3 wrapper. Single file at ``<cache_root>/index.sqlite``.

    Not thread-safe across writers — pywebview JS bridge calls land on
    pywebview's worker thread, so wrap critical writes in ``with db.tx()``
    if invoked from multiple coroutines. For the current single-threaded
    UI flow that's not strictly necessary.
    """

    def __init__(self, db_path: Path):
        self.path = Path(db_path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._conn = self._open_or_recreate(self.path)

    # ---- connection mgmt ----

    @staticmethod
    def _open_or_recreate(path: Path) -> sqlite3.Connection:
        # Step 1: try opening existing file. If integrity_check fails, back
        # it up and recreate from scratch (per spec).
        if path.exists():
            try:
                conn = sqlite3.connect(str(path), isolation_level=None)
                conn.row_factory = sqlite3.Row
                row = conn.execute("PRAGMA integrity_check").fetchone()
                if row is None or str(row[0]).lower() != "ok":
                    raise sqlite3.DatabaseError(f"integrity check returned {row}")
                conn.executescript(SCHEMA_SQL)  # idempotent
                return conn
            except sqlite3.DatabaseError as exc:
                # Close the conn before renaming; on Windows an open handle
                # blocks Path.replace.
                try:
                    conn.close()
                except Exception:
                    pass
                bak = path.with_suffix(path.suffix + f".bak.{int(time.time())}")
                try:
                    path.replace(bak)
                except OSError:
                    # If the move failed (e.g. perms), fall through to
                    # unlinking — we still want a clean slate.
                    try:
                        path.unlink()
                    except OSError:
                        pass
                # Log to stderr — there's no formal logger here yet.
                import sys
                print(
                    f"[log_center.index] WARN: sqlite integrity_check failed ({exc}); "
                    f"moved corrupt db to {bak}, recreating empty",
                    file=sys.stderr,
                )
        conn = sqlite3.connect(str(path), isolation_level=None)
        conn.row_factory = sqlite3.Row
        conn.executescript(SCHEMA_SQL)
        return conn

    def close(self) -> None:
        try:
            self._conn.close()
        except Exception:
            pass

    @contextmanager
    def tx(self) -> Iterator[sqlite3.Connection]:
        """Exclusive transaction context. Rollback on exception."""
        self._conn.execute("BEGIN IMMEDIATE")
        try:
            yield self._conn
            self._conn.execute("COMMIT")
        except Exception:
            self._conn.execute("ROLLBACK")
            raise

    # ---- write ops ----

    def upsert_records(self, records: list[dict | FeedbackRow]) -> tuple[int, int]:
        """Insert or update ``records``. Returns (inserted, updated).

        Preserves ``downloaded`` / ``download_path`` / ``notes`` on update —
        those columns live only locally.
        """
        inserted = 0
        updated = 0
        with self.tx() as conn:
            for raw in records:
                if isinstance(raw, FeedbackRow):
                    rec = raw.to_dict()
                else:
                    rec = dict(raw)
                rec.setdefault("downloaded", 0)
                rec.setdefault("download_path", None)
                rec.setdefault("notes", None)
                # Force types
                rec["size"] = int(rec.get("size") or 0)
                row = conn.execute("SELECT id FROM feedback WHERE id = ?", (rec["id"],)).fetchone()
                if row is None:
                    conn.execute(
                        """
                        INSERT INTO feedback
                          (id, region, ts, app_version, os, gpu, summary, email, size,
                           bucket_path, downloaded, download_path, notes)
                        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                        """,
                        (
                            rec["id"], rec["region"], rec["ts"],
                            rec.get("app_version"), rec.get("os"), rec.get("gpu"),
                            rec.get("summary"), rec.get("email"), rec["size"],
                            rec["bucket_path"], int(rec.get("downloaded") or 0),
                            rec.get("download_path"), rec.get("notes"),
                        ),
                    )
                    inserted += 1
                else:
                    # Update server-sourced fields only; keep local-only columns intact.
                    conn.execute(
                        """
                        UPDATE feedback
                           SET region = ?, ts = ?, app_version = ?, os = ?, gpu = ?,
                               summary = ?, email = ?, size = ?, bucket_path = ?
                         WHERE id = ?
                        """,
                        (
                            rec["region"], rec["ts"], rec.get("app_version"), rec.get("os"),
                            rec.get("gpu"), rec.get("summary"), rec.get("email"),
                            rec["size"], rec["bucket_path"], rec["id"],
                        ),
                    )
                    updated += 1
        return inserted, updated

    def update_download(self, id_: str, download_path: Optional[str]) -> None:
        with self.tx() as conn:
            conn.execute(
                "UPDATE feedback SET downloaded = ?, download_path = ? WHERE id = ?",
                (1 if download_path else 0, download_path, id_),
            )

    def update_notes(self, id_: str, text: Optional[str]) -> None:
        with self.tx() as conn:
            conn.execute("UPDATE feedback SET notes = ? WHERE id = ?", (text, id_))

    def delete(self, id_: str) -> None:
        with self.tx() as conn:
            conn.execute("DELETE FROM feedback WHERE id = ?", (id_,))

    # ---- read ops ----

    def get(self, id_: str) -> Optional[dict]:
        row = self._conn.execute("SELECT * FROM feedback WHERE id = ?", (id_,)).fetchone()
        return dict(row) if row else None

    def list(
        self,
        *,
        since: Optional[str] = None,
        until: Optional[str] = None,
        region: Optional[str] = None,
        downloaded: Optional[bool] = None,
        limit: int = 1000,
    ) -> list[dict]:
        """Return rows matching filters, ts DESC.

        ``since`` / ``until`` are ISO 8601 strings; sqlite compares them
        lexicographically which works for ISO 8601 with consistent zone.
        """
        clauses: list[str] = []
        params: list[Any] = []
        if since:
            clauses.append("ts >= ?")
            params.append(since)
        if until:
            clauses.append("ts <= ?")
            params.append(until)
        if region and region != "all":
            clauses.append("region = ?")
            params.append(region)
        if downloaded is True:
            clauses.append("downloaded = 1")
        elif downloaded is False:
            clauses.append("downloaded = 0")
        where = ("WHERE " + " AND ".join(clauses)) if clauses else ""
        sql = f"SELECT * FROM feedback {where} ORDER BY ts DESC LIMIT ?"
        params.append(int(limit))
        rows = self._conn.execute(sql, params).fetchall()
        return [dict(r) for r in rows]

    def all_ids_with_download(self) -> list[tuple[str, Optional[str]]]:
        """Return (id, download_path) pairs for every downloaded row."""
        rows = self._conn.execute(
            "SELECT id, download_path FROM feedback WHERE downloaded = 1"
        ).fetchall()
        return [(r["id"], r["download_path"]) for r in rows]
