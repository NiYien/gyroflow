"""BackendAPI — the object exposed to JS through pywebview's bridge.

Every method returns ``{"ok": bool, "data": ..., "error": ...}`` so the
JS layer can render the result without try/except gymnastics.

The constructor accepts already-built sub-clients so unit tests can swap
mocks in without touching the real R2 / 123 / KV / niyien services.
"""

from __future__ import annotations

import json
import sys
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from .api import AuthError, NiyenApiClient, ServerError
from .config import Config
from .helpers import (
    clipboard_set,
    directory_size,
    extract_zip,
    find_first_existing,
    human_size,
    open_in_file_manager,
    safe_rmtree,
)
from .index import IndexDB
from .kv import UpstashKvClient, UpstashKvError
from .pan123 import Pan123Client, Pan123Error
from .r2 import R2Client


PROMPT_TEMPLATE_PATH = Path(__file__).resolve().parent.parent / "templates" / "analyze.md"


def _ok(data: Any = None) -> dict[str, Any]:
    return {"ok": True, "data": data}


def _err(message: str) -> dict[str, Any]:
    return {"ok": False, "error": str(message)}


def _bucket_path_to_local(cache_root: Path, ts: str, id_: str) -> Path:
    """Resolve cache layout: ``<cache_root>/<yyyy-mm-dd>/<id>/``.

    ``ts`` is the server-side ISO-8601 timestamp. We extract the date by
    parsing the timestamp directly, falling back to the id prefix
    ``YYYYMMDD-...`` if that fails.
    """
    day = None
    try:
        day = datetime.fromisoformat(ts.replace("Z", "+00:00")).date().isoformat()
    except Exception:
        pass
    if not day and len(id_) >= 8 and id_[:8].isdigit():
        day = f"{id_[:4]}-{id_[4:6]}-{id_[6:8]}"
    if not day:
        day = "unknown"
    return cache_root / day / id_


def _date_key_from_id(id_: str) -> str:
    """``20260502-8a2f1c3d`` → ``20260502``. Returns "" on bad shape."""
    head = id_.split("-", 1)[0] if "-" in id_ else id_[:8]
    return head if head.isdigit() and len(head) == 8 else ""


class BackendAPI:
    """Public surface for the JS frontend. Keep methods small and JSON-safe."""

    def __init__(
        self,
        config: Config,
        *,
        index_db: Optional[IndexDB] = None,
        niyien: Optional[NiyenApiClient] = None,
        r2: Optional[R2Client] = None,
        pan123: Optional[Pan123Client] = None,
        kv: Optional[UpstashKvClient] = None,
    ):
        self.config = config
        self.cache_root = Path(config.cache_root)
        self.cache_root.mkdir(parents=True, exist_ok=True)

        self.db = index_db or IndexDB(self.cache_root.parent / "index.sqlite")
        self.niyien = niyien or NiyenApiClient(
            base_url=config.niyien_api_base,
            admin_token=config.feedback_admin_token,
        )
        self.r2 = r2 or R2Client(
            account_id=config.r2.account_id,
            access_key_id=config.r2.access_key_id,
            secret_access_key=config.r2.secret_access_key,
            bucket=config.r2.bucket,
        )
        self.pan123 = pan123 or Pan123Client(
            client_id=config.pan123.client_id,
            client_secret=config.pan123.client_secret,
            feedback_root=config.pan123.feedback_root_dir,
        )
        self.kv = kv or UpstashKvClient(
            rest_url=config.upstash_kv.url,
            rest_token=config.upstash_kv.token,
        )

        # Single lock per row to keep concurrent download/delete from
        # tripping over each other; pywebview generally serializes JS->Py
        # calls but be safe.
        self._row_locks: dict[str, threading.Lock] = {}
        self._row_locks_guard = threading.Lock()

    # ---------------- internals ----------------

    def _row_lock(self, id_: str) -> threading.Lock:
        with self._row_locks_guard:
            lock = self._row_locks.get(id_)
            if lock is None:
                lock = threading.Lock()
                self._row_locks[id_] = lock
            return lock

    def _get_row(self, id_: str) -> Optional[dict]:
        return self.db.get(id_)

    def _delete_remote_object(self, row: dict) -> list[str]:
        """Delete the underlying object on R2 / 123. Returns a list of
        failure messages (empty == fully succeeded).
        """
        failures: list[str] = []
        region = row.get("region")
        bucket_path = row.get("bucket_path") or ""
        if not bucket_path:
            failures.append("missing bucket_path")
            return failures
        if region == "global":
            try:
                self.r2.delete(bucket_path)
            except Exception as exc:
                failures.append(f"R2 delete failed: {exc}")
        elif region == "cn":
            try:
                self.pan123.delete(bucket_path)
            except Pan123Error as exc:
                failures.append(f"123 delete failed: {exc}")
            except Exception as exc:
                failures.append(f"123 delete failed: {exc}")
        else:
            failures.append(f"unknown region: {region!r}")
        return failures

    def _delete_kv(self, id_: str) -> list[str]:
        failures: list[str] = []
        try:
            self.kv.delete(f"fb:{id_}")
        except UpstashKvError as exc:
            failures.append(f"KV DEL fb:{id_} failed: {exc}")
        date_key = _date_key_from_id(id_)
        if date_key:
            try:
                self.kv.lrem(f"fb:index:{date_key}", id_, count=0)
            except UpstashKvError as exc:
                failures.append(f"KV LREM fb:index:{date_key} failed: {exc}")
        return failures

    # ---------------- public methods (called from JS) ----------------

    def ping(self) -> dict[str, Any]:
        """Smoke method: verifies the JS bridge wiring."""
        return _ok({
            "config_path": str(self.config.config_path),
            "cache_root": str(self.cache_root),
            "niyien_api_base": self.config.niyien_api_base,
        })

    def refresh(self, since_iso: Optional[str] = None, limit: int = 500) -> dict[str, Any]:
        """Pull the index from /api/feedback/list and upsert into sqlite."""
        try:
            since = (
                datetime.fromisoformat(since_iso.replace("Z", "+00:00"))
                if since_iso
                else None
            )
        except Exception:
            return _err(f"invalid since_iso: {since_iso!r}")
        try:
            items = self.niyien.list_feedback(since=since, limit=int(limit))
        except AuthError as exc:
            return _err(f"auth: {exc}")
        except ServerError as exc:
            return _err(f"server: {exc}")
        except Exception as exc:  # noqa: BLE001
            return _err(f"unexpected: {exc}")

        # Coerce server fields to our sqlite columns.
        records: list[dict] = []
        for it in items:
            if not isinstance(it, dict):
                continue
            rec = {
                "id": it.get("id"),
                "region": it.get("region") or "global",
                "ts": it.get("ts") or it.get("confirmed_ts") or "",
                "app_version": it.get("app_version"),
                "os": it.get("os"),
                "gpu": it.get("gpu"),
                "summary": it.get("summary"),
                "email": it.get("email"),
                "size": int(it.get("size") or 0),
                "bucket_path": it.get("bucket_path") or "",
            }
            if not rec["id"] or not rec["bucket_path"]:
                continue
            records.append(rec)
        inserted, updated = self.db.upsert_records(records)
        return _ok({
            "inserted": inserted,
            "updated": updated,
            "total_fetched": len(items),
            "kept": len(records),
        })

    def list(self, filters: Optional[dict] = None) -> dict[str, Any]:
        f = dict(filters or {})
        try:
            rows = self.db.list(
                since=f.get("since") or None,
                until=f.get("until") or None,
                region=f.get("region") or None,
                downloaded=(
                    True if f.get("downloaded") == "yes"
                    else False if f.get("downloaded") == "no"
                    else None
                ),
                limit=int(f.get("limit") or 1000),
            )
        except Exception as exc:  # noqa: BLE001
            return _err(f"list failed: {exc}")
        # Annotate each row with a human size for the table.
        for r in rows:
            r["size_human"] = human_size(int(r.get("size") or 0))
        return _ok(rows)

    def download_one(self, id_: str, force: bool = False) -> dict[str, Any]:
        with self._row_lock(id_):
            row = self._get_row(id_)
            if not row:
                return _err(f"unknown id: {id_}")
            extracted_dir = _bucket_path_to_local(self.cache_root, row["ts"], row["id"])
            if extracted_dir.exists() and not force:
                return _err("already downloaded; pass force=true to redownload")
            if extracted_dir.exists() and force:
                if not safe_rmtree(extracted_dir):
                    return _err(f"could not clear existing dir: {extracted_dir}")

            zip_path = extracted_dir.with_suffix(".zip")
            zip_path.parent.mkdir(parents=True, exist_ok=True)
            try:
                if row["region"] == "global":
                    self.r2.download(row["bucket_path"], zip_path)
                elif row["region"] == "cn":
                    self.pan123.download(row["bucket_path"], zip_path)
                else:
                    return _err(f"unknown region: {row['region']!r}")
            except Exception as exc:  # noqa: BLE001
                # Clean up any partial zip file.
                try:
                    zip_path.unlink(missing_ok=True)
                except Exception:
                    pass
                return _err(f"download failed: {exc}")

            # Extract + drop the zip.
            try:
                extract_zip(zip_path, extracted_dir)
            except Exception as exc:  # noqa: BLE001
                return _err(f"extract failed: {exc}")
            try:
                zip_path.unlink(missing_ok=True)
            except Exception:
                pass

            self.db.update_download(id_, str(extracted_dir))
            return _ok({
                "id": id_,
                "download_path": str(extracted_dir),
                "size_human": human_size(directory_size(extracted_dir)),
            })

    def open_local(self, id_: str) -> dict[str, Any]:
        row = self._get_row(id_)
        if not row:
            return _err(f"unknown id: {id_}")
        path = row.get("download_path")
        if not path or not Path(path).exists():
            return _err("not downloaded yet")
        open_in_file_manager(Path(path))
        return _ok(path)

    def delete_one(self, id_: str) -> dict[str, Any]:
        with self._row_lock(id_):
            row = self._get_row(id_)
            if not row:
                return _err(f"unknown id: {id_}")
            failures: list[str] = []
            failures.extend(self._delete_remote_object(row))
            failures.extend(self._delete_kv(id_))
            local = row.get("download_path")
            if local:
                if not safe_rmtree(Path(local)):
                    failures.append(f"local rmtree failed: {local}")
            self.db.delete(id_)
            return _ok({"id": id_, "failures": failures})

    def delete_many(self, ids: list[str]) -> dict[str, Any]:
        if not ids:
            return _err("ids list empty")
        succeeded = 0
        failed: list[dict] = []
        for id_ in list(ids):
            res = self.delete_one(str(id_))
            if not res.get("ok"):
                failed.append({"id": id_, "error": res.get("error")})
            elif res.get("data", {}).get("failures"):
                failed.append({"id": id_, "error": "; ".join(res["data"]["failures"])})
                succeeded += 1
            else:
                succeeded += 1
        return _ok({"total": len(ids), "succeeded": succeeded, "failed": failed})

    def update_notes(self, id_: str, text: str) -> dict[str, Any]:
        row = self._get_row(id_)
        if not row:
            return _err(f"unknown id: {id_}")
        self.db.update_notes(id_, text or None)
        return _ok(True)

    def get_cache_size(self) -> dict[str, Any]:
        size = directory_size(self.cache_root)
        return _ok({"bytes": size, "human": human_size(size)})

    def clean_cache(self, threshold_days: int) -> dict[str, Any]:
        """Remove extracted directories whose row's ts is older than
        ``threshold_days``. Updates sqlite to mark those rows
        ``downloaded=0, download_path=NULL``.
        """
        try:
            days = int(threshold_days)
        except (TypeError, ValueError):
            return _err(f"invalid threshold_days: {threshold_days!r}")
        if days < 0:
            return _err("threshold_days must be >= 0")
        cutoff = datetime.now(timezone.utc).timestamp() - days * 86400
        cleaned = 0
        for id_, path in self.db.all_ids_with_download():
            row = self.db.get(id_)
            if not row:
                continue
            try:
                row_ts = datetime.fromisoformat(row["ts"].replace("Z", "+00:00")).timestamp()
            except Exception:
                continue
            if row_ts > cutoff:
                continue
            if path and Path(path).exists():
                if not safe_rmtree(Path(path)):
                    continue
            self.db.update_download(id_, None)
            cleaned += 1
        return _ok({"cleaned": cleaned})

    def copy_prompt(self, id_: str) -> dict[str, Any]:
        """Render the analyze template and copy it to the system clipboard."""
        row = self._get_row(id_)
        if not row:
            return _err(f"unknown id: {id_}")
        path_str = row.get("download_path")
        if not path_str or not Path(path_str).exists():
            return _err("download the row first")
        if not PROMPT_TEMPLATE_PATH.exists():
            return _err(f"missing template: {PROMPT_TEMPLATE_PATH}")
        root = Path(path_str)
        # Pull manifest fields if present, otherwise use sqlite values.
        manifest_path = find_first_existing(root, ["manifest.json"])
        manifest: dict = {}
        if manifest_path:
            try:
                manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
                if not isinstance(manifest, dict):
                    manifest = {}
            except Exception:
                manifest = {}

        substitutions = {
            "feedback_dir": str(root),
            "user_summary": (
                manifest.get("summary") or row.get("summary") or "(no summary)"
            ),
            "app_version": manifest.get("app_version") or row.get("app_version") or "(unknown)",
            "os": manifest.get("os") or row.get("os") or "(unknown)",
            "gpu": manifest.get("gpu") or row.get("gpu") or "(unknown)",
        }
        try:
            template = PROMPT_TEMPLATE_PATH.read_text(encoding="utf-8")
            rendered = template.format(**substitutions)
        except KeyError as exc:
            return _err(f"template references unknown placeholder: {exc}")
        except Exception as exc:  # noqa: BLE001
            return _err(f"template render failed: {exc}")

        mechanism = clipboard_set(rendered, fallback_dir=self.cache_root.parent)
        return _ok({
            "id": id_,
            "chars": len(rendered),
            "mechanism": mechanism,
            # Include preview for UI debug; trim to 280 chars.
            "preview": rendered[:280],
        })

    def shutdown(self) -> dict[str, Any]:
        """Called by the entry point when the window is closing."""
        try:
            self.db.close()
        except Exception as exc:
            print(f"[log_center.orchestrator] db.close failed: {exc}", file=sys.stderr)
        return _ok(True)
