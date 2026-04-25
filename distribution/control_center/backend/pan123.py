"""Pan123 (123 网盘) lightweight client + task runner for the control center.

This module owns:

1. A small Pan123Client subset (auth + directory listing) used by the
   inventory-status panel — we don't need the full upload/dedup logic
   here because the heavy lifting still happens in
   `_scripts/publish_pan123_release.py`.

2. A TaskRegistry singleton that spawns the publish script as a
   subprocess, parses its JSONL progress events, and queues them so
   the frontend can poll for incremental updates.
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import sys
import threading
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

import requests

from .helpers import build_proxy_mapping, normalize_proxy_url

PROGRESS_EVENT_PREFIX = "@@CC_EVENT@@"
DEFAULT_API_BASE = "https://open-api.123pan.com"


# ----------------------------- Pan123 lightweight client -----------------------------


class Pan123Client:
    """Auth + list_directory only — sufficient for inventory queries.

    The publish script has its own (heavier) client for uploads. This is
    deliberately minimal so the dashboard doesn't pull megabytes of
    upload-side state into the API process.

    Note: 123 网盘 traffic intentionally bypasses the configured network
    proxy (which is meant for GFW-bypass GitHub access). Routing 123
    through that proxy slows downloads and may trigger spurious failures.
    proxy_url is accepted for backward-compatibility but not applied.
    """

    def __init__(self, client_id: str, client_secret: str, proxy_url: str = ""):
        self.client_id = (client_id or "").strip()
        self.client_secret = (client_secret or "").strip()
        # Kept for API compatibility; not used. See class docstring.
        self.proxy_url = normalize_proxy_url(proxy_url)
        self._token: Optional[str] = None
        self._token_exp: float = 0.0

    def _request_kwargs(self, *, timeout: int) -> dict:
        # Direct connection to open-api.123pan.com — see class docstring
        # for rationale. Mirrors `Pan123Client.session.trust_env = False`
        # in _scripts/publish_pan123_release.py.
        return {"timeout": timeout, "proxies": {"http": None, "https": None}}

    def _ensure_token(self) -> str:
        if self._token and time.time() < self._token_exp - 60:
            return self._token
        if not self.client_id or not self.client_secret:
            raise RuntimeError("Missing pan123 client_id/secret")
        url = f"{DEFAULT_API_BASE}/api/v1/access_token"
        body = {"clientID": self.client_id, "clientSecret": self.client_secret}
        r = requests.post(url, json=body, headers={"Platform": "open_platform"}, **self._request_kwargs(timeout=30))
        r.raise_for_status()
        data = r.json()
        if data.get("code") != 0:
            raise RuntimeError(f"pan123 auth failed: {data.get('message') or data}")
        token = (data.get("data") or {}).get("accessToken")
        expired_at = (data.get("data") or {}).get("expiredAt", "")
        if not token:
            raise RuntimeError(f"pan123 auth: no accessToken in {data}")
        self._token = str(token)
        # expiredAt is ISO-ish; fall back to 1h if parsing fails
        try:
            from datetime import datetime
            self._token_exp = datetime.fromisoformat(expired_at.replace("Z", "+00:00")).timestamp()
        except Exception:
            self._token_exp = time.time() + 3600
        return self._token

    def _headers(self) -> dict:
        return {
            "Authorization": f"Bearer {self._ensure_token()}",
            "Platform": "open_platform",
            "Content-Type": "application/json",
        }

    @staticmethod
    def _entry_id(entry: dict) -> int:
        return int(entry.get("fileID") or entry.get("fileId") or entry.get("id") or 0)

    def list_directory(self, parent_id: int, *, page_size: int = 100) -> list[dict]:
        """Return all entries under parent_id, paging through lastFileId.

        Dedupe by fileID — the v2 API has been observed returning overlapping
        page boundaries when a directory was modified mid-scan, which would
        otherwise show the same entry twice on the dashboard.
        """
        out: list[dict] = []
        seen_ids: set[int] = set()
        last_file_id = 0
        # Hard ceiling to prevent runaway loops on misbehaving pagination
        for _ in range(50):
            params = {
                "parentFileId": int(parent_id),
                "limit": int(page_size),
                "lastFileId": int(last_file_id),
            }
            r = requests.get(
                f"{DEFAULT_API_BASE}/api/v2/file/list",
                params=params,
                headers=self._headers(),
                **self._request_kwargs(timeout=30),
            )
            r.raise_for_status()
            data = r.json()
            if data.get("code") != 0:
                raise RuntimeError(f"pan123 list failed: {data.get('message') or data}")
            payload = data.get("data") or {}
            entries = payload.get("fileList") or []
            new_in_page = 0
            for entry in entries:
                fid = self._entry_id(entry)
                if fid and fid in seen_ids:
                    continue
                if fid:
                    seen_ids.add(fid)
                out.append(entry)
                new_in_page += 1
            next_last = int(payload.get("lastFileId", -1) or -1)
            if next_last == -1 or not entries or new_in_page == 0:
                break
            if next_last == last_file_id:
                # API stuck on same page — bail to avoid infinite loop
                break
            last_file_id = next_last
        return out

    def directory_total_size(self, parent_id: int, *, max_depth: int = 4) -> tuple[int, int]:
        """Recursively sum (file_size_bytes, file_count) under parent_id.

        Stops at max_depth to keep scans bounded — content bundles are at most
        2-3 levels deep (root / category / file).
        """
        if max_depth <= 0:
            return 0, 0
        total_size = 0
        total_files = 0
        for entry in self.list_directory(parent_id):
            etype = int(entry.get("type", -1))
            if etype == 0:
                total_size += int(entry.get("size", 0) or 0)
                total_files += 1
            elif etype == 1:
                sub_id = self._entry_id(entry)
                if sub_id:
                    s, c = self.directory_total_size(sub_id, max_depth=max_depth - 1)
                    total_size += s
                    total_files += c
        return total_size, total_files

    def get_download_url(self, file_id: int) -> str:
        """Resolve a download URL for a file_id via GET /api/v1/file/download_info.

        Notes:
        - POST variant returns 404 on this endpoint (verified empirically),
          so we only call GET.
        - 123 开放平台 sometimes returns "文件不存在" even when the file ID
          was just listed — this is a known quirk where the open API
          download endpoint requires the file to be in the same account's
          personal storage AND that the token has download scope. Files
          uploaded by the publish script with the same token should work.
        """
        if int(file_id or 0) <= 0:
            raise RuntimeError("get_download_url: invalid file_id <= 0")
        try:
            r = requests.get(
                f"{DEFAULT_API_BASE}/api/v1/file/download_info",
                params={"fileId": int(file_id)},
                headers=self._headers(),
                **self._request_kwargs(timeout=30),
            )
            r.raise_for_status()
            data = r.json()
        except Exception as e:
            raise RuntimeError(f"pan123 download_info HTTP error for fileId={file_id}: {e}")
        if data.get("code") != 0:
            msg = data.get("message") or str(data)
            raise RuntimeError(f"pan123 download_info: {msg} (fileId={file_id})")
        url = (data.get("data") or {}).get("downloadUrl", "")
        if not url:
            raise RuntimeError(f"pan123 download_info: empty downloadUrl (fileId={file_id})")
        return str(url)

    def fetch_file_text(self, file_id: int, *, max_bytes: int = 256 * 1024) -> str:
        """Download a small file (e.g. a JSON manifest) and return its text.

        Caps the response at max_bytes to avoid runaway downloads if the
        endpoint surprises us.
        """
        url = self.get_download_url(file_id)
        r = requests.get(url, **self._request_kwargs(timeout=60), stream=True)
        r.raise_for_status()
        chunks = []
        total = 0
        for chunk in r.iter_content(chunk_size=8192):
            if not chunk:
                continue
            total += len(chunk)
            if total > max_bytes:
                raise RuntimeError(f"pan123 fetch_file_text: file too large (>{max_bytes} bytes)")
            chunks.append(chunk)
        return b"".join(chunks).decode("utf-8", errors="replace")

    def find_child(self, parent_id: int, name: str, *, is_dir: bool) -> Optional[dict]:
        """Return the matching child entry or None.

        type=1 directory, type=0 file in the v2 listing schema.
        """
        wanted_type = 1 if is_dir else 0
        for entry in self.list_directory(parent_id):
            if str(entry.get("filename") or entry.get("name") or "").strip() == name:
                if int(entry.get("type", -1)) == wanted_type:
                    return entry
        return None


# ----------------------------- Task registry (publish-with-progress) -----------------------------


@dataclass
class _Task:
    token: str
    queue: "queue.Queue[dict]" = field(default_factory=queue.Queue)
    finished: bool = False
    ok: Optional[bool] = None  # None=running, True=success, False=error
    error: str = ""
    process: Optional[subprocess.Popen] = None
    started_at: float = field(default_factory=time.time)
    finished_at: float = 0.0
    final_summary: Optional[dict] = None
    cancelled: bool = False


class TaskRegistry:
    """Single-task-at-a-time runner for pan123 publishes.

    The frontend polls poll(token) to drain the event queue; events are
    `{type, ts, ...}` dicts (`type` ∈ status / log / progress / success /
    error / finished / cancelled). The frontend stops polling on
    finished=True.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._tasks: dict[str, _Task] = {}
        self._active_token: Optional[str] = None

    # ---- public API ----

    def active_token(self) -> Optional[str]:
        with self._lock:
            return self._active_token

    def submit(self, runner, *, on_success=None, on_error=None) -> dict:
        """Start a worker thread. Returns {ok, token, error}.

        `runner` is a callable taking (reporter, task) — it should call
        reporter.* methods and return the final summary dict (or raise).

        `on_success(summary)` runs (in the worker thread, after `success`
        but before `finished`) when the task completes cleanly. It's the
        hook execute_app_action uses to push policy + deploy hook ONLY
        after pan123 upload finishes — eliminating the manifest-vs-upload
        race condition where cn clients saw a new manifest before 123 had
        the files.

        `on_error(error_message)` runs on failure. Both callbacks are
        wrapped in try/except so a callback bug doesn't corrupt task
        state.

        Refuses if another task is still running (single-task model).
        """
        with self._lock:
            if self._active_token and not self._tasks[self._active_token].finished:
                return {"ok": False, "error": "已有任务在运行,等它结束或取消后再发起", "active_token": self._active_token}
            token = uuid.uuid4().hex
            task = _Task(token=token)
            self._tasks[token] = task
            self._active_token = token

        reporter = _Reporter(task)

        def _wrap():
            import traceback as _tb
            try:
                summary = runner(reporter, task)
                task.final_summary = summary if isinstance(summary, dict) else {"summary": summary}
                task.ok = True
                reporter.success(summary=task.final_summary)
                if callable(on_success):
                    try:
                        on_success(task.final_summary)
                    except Exception as cb_err:
                        # Don't drop the success — task succeeded — but
                        # surface the callback failure so the operator
                        # sees that the post-step (policy push) didn't run.
                        reporter.error(
                            message=f"post-success callback failed: {cb_err}",
                            detail=_tb.format_exc(),
                        )
                        # Mark task as not-ok so frontend treats it as a
                        # publish failure (manifest didn't get pushed).
                        task.ok = False
                        task.error = f"post-success callback failed: {cb_err}"
            except Exception as exc:  # noqa: BLE001
                task.error = str(exc)
                task.ok = False
                reporter.error(message=str(exc), detail=_tb.format_exc())
                if callable(on_error):
                    try:
                        on_error(str(exc))
                    except Exception:
                        pass
            finally:
                task.finished = True
                task.finished_at = time.time()
                reporter.finished()
                with self._lock:
                    if self._active_token == token:
                        self._active_token = None

        # If Thread.start() itself fails (rare — typically RuntimeError when
        # the runtime can't allocate a thread), the registry would be wedged
        # forever because _active_token is set but no _wrap will ever clear
        # it. Reset on failure so the operator can retry.
        try:
            threading.Thread(target=_wrap, name=f"pan123-task-{token[:8]}", daemon=True).start()
        except Exception as e:
            with self._lock:
                if self._active_token == token:
                    self._active_token = None
                self._tasks.pop(token, None)
            return {"ok": False, "error": f"thread start failed: {e}"}
        return {"ok": True, "token": token}

    def poll(self, token: str, *, max_events: int = 200) -> dict:
        task = self._tasks.get(str(token).strip())
        if not task:
            return {"ok": False, "error": "unknown token", "finished": True}
        events: list[dict] = []
        for _ in range(max_events):
            try:
                events.append(task.queue.get_nowait())
            except queue.Empty:
                break
        return {
            "ok": True,
            "events": events,
            "finished": task.finished,
            "task_ok": task.ok,
            "error": task.error,
            "summary": task.final_summary,
            "elapsed_s": (task.finished_at or time.time()) - task.started_at,
        }

    def cancel(self, token: str) -> dict:
        task = self._tasks.get(str(token).strip())
        if not task:
            return {"ok": False, "error": "unknown token"}
        if task.finished:
            return {"ok": True, "message": "task already finished"}
        task.cancelled = True
        if task.process is not None:
            try:
                task.process.terminate()
            except Exception:
                pass
        return {"ok": True, "message": "cancel signal sent"}

    def cancel_all(self, *, kill_after_s: float = 3.0) -> int:
        """Forcibly stop every running task — used on app shutdown.

        Sends terminate, waits briefly, then kill if still running. Returns
        the number of tasks signalled.
        """
        with self._lock:
            tasks = [t for t in self._tasks.values() if not t.finished]
        for task in tasks:
            task.cancelled = True
            if task.process is not None:
                try:
                    task.process.terminate()
                except Exception:
                    pass
        if not tasks:
            return 0
        # Wait a moment then escalate to kill — Windows subprocesses often
        # ignore terminate() when blocked on socket I/O.
        deadline = time.time() + max(kill_after_s, 0.0)
        while time.time() < deadline:
            still_running = [t for t in tasks if t.process and t.process.poll() is None]
            if not still_running:
                break
            time.sleep(0.1)
        for task in tasks:
            if task.process and task.process.poll() is None:
                try:
                    task.process.kill()
                except Exception:
                    pass
        return len(tasks)


class _Reporter:
    """Worker-side handle. Pushes events into the task queue."""

    def __init__(self, task: _Task) -> None:
        self._task = task

    def _emit(self, event_type: str, **payload: Any) -> None:
        evt = {"type": event_type, "ts": time.time(), **payload}
        self._task.queue.put(evt)

    def status(self, message: str) -> None:
        self._emit("status", message=str(message))

    def log(self, message: str) -> None:
        self._emit("log", message=str(message))

    def progress(self, *, phase: str = "", label: str = "", message: str = "",
                 current: Optional[int] = None, total: Optional[int] = None,
                 mode: str = "") -> None:
        payload = {"phase": phase, "label": label, "message": message, "mode": mode}
        if current is not None:
            payload["current"] = int(current)
        if total is not None:
            payload["total"] = int(total)
        self._emit("progress", **payload)

    def success(self, *, summary: Any = None) -> None:
        self._emit("success", summary=summary)

    def error(self, *, message: str, detail: str = "") -> None:
        self._emit("error", message=str(message), detail=str(detail))

    def finished(self) -> None:
        self._emit("finished")


# ----------------------------- subprocess runner -----------------------------


def run_publish_subprocess(reporter: _Reporter, task: _Task, *,
                           script_path: Path, command: list[str],
                           cwd: Path, env: dict, stdout_log_path: Path) -> dict:
    """Spawn publish_pan123_release.py with NIYIEN_PROGRESS_MODE=jsonl,
    parse stdout JSONL events, push them through `reporter`.

    Returns a small summary dict. Raises RuntimeError on non-zero exit.
    """
    if not script_path.exists():
        raise RuntimeError(f"publish script missing: {script_path}")

    env = dict(env)
    env["NIYIEN_PROGRESS_MODE"] = "jsonl"
    env["PYTHONIOENCODING"] = "utf-8"
    env["PYTHONUTF8"] = "1"

    reporter.status("启动 publish_pan123 脚本")
    reporter.log(f"command: {' '.join(command)}")
    reporter.log(f"stdout log: {stdout_log_path}")

    stdout_log_path.parent.mkdir(parents=True, exist_ok=True)
    # On Windows + pythonw.exe (control_center.pyw), spawning subprocesses
    # creates a brief console flicker. Suppress with CREATE_NO_WINDOW.
    extra: dict = {}
    if sys.platform == "win32":
        extra["creationflags"] = 0x08000000  # CREATE_NO_WINDOW
    with stdout_log_path.open("w", encoding="utf-8") as log_fh:
        process = subprocess.Popen(
            command,
            cwd=str(cwd),
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
            **extra,
        )
        task.process = process
        last_lines: list[str] = []
        # Captured `phase=finalize_summary` event from the publish script.
        # Carries content_tag / lens_version / lens_sha256 etc. that are
        # only known at script-runtime — control_center then uses these
        # to backfill policy entry + Vercel envs in the finalize step.
        finalize_summary: dict = {}
        if process.stdout is not None:
            for raw_line in process.stdout:
                if task.cancelled:
                    process.terminate()
                    break
                line = raw_line.rstrip()
                if not line:
                    continue
                log_fh.write(line + "\n")
                log_fh.flush()
                if line.startswith(PROGRESS_EVENT_PREFIX):
                    try:
                        evt = json.loads(line[len(PROGRESS_EVENT_PREFIX):])
                    except json.JSONDecodeError:
                        evt = None
                    if isinstance(evt, dict):
                        if str(evt.get("phase", "")) == "finalize_summary":
                            # Stash for caller; don't render as a progress tick.
                            finalize_summary = {
                                k: v for k, v in evt.items() if k != "phase"
                            }
                            reporter.log(f"finalize_summary captured: content_tag={finalize_summary.get('content_tag')}")
                            continue
                        reporter.progress(
                            phase=str(evt.get("phase", "")),
                            label=str(evt.get("label", "")),
                            message=str(evt.get("message", "")),
                            current=evt.get("current"),
                            total=evt.get("total"),
                            mode=str(evt.get("mode", "")),
                        )
                        continue
                # Plain log line
                reporter.log(line)
                last_lines.append(line)
                if len(last_lines) > 30:
                    last_lines = last_lines[-30:]
        rc = process.wait()
    if task.cancelled:
        raise RuntimeError("用户取消")
    if rc != 0:
        tail = "\n".join(last_lines[-12:]).strip() or "(no tail output)"
        raise RuntimeError(f"publish 脚本退出码 {rc}\n--- stdout 末尾 ---\n{tail}")
    return {
        "return_code": rc,
        "stdout_log": str(stdout_log_path),
        "finalize_summary": finalize_summary,
    }


# Module-level singleton
TASKS = TaskRegistry()


# ----------------------------- Bundle metadata cache -----------------------------
# content_tag is a sha256[:12] hash → file content is immutable, so once we've
# resolved the manifest fields for a tag we never need to re-fetch them.

_CACHE_PATH = Path(__file__).resolve().parent.parent / "_cache" / "pan123_bundles.json"


def load_bundle_cache() -> dict:
    """Return {content_tag: {app_tag, lens_release_tag, ...}} or empty dict."""
    if not _CACHE_PATH.exists():
        return {}
    try:
        data = json.loads(_CACHE_PATH.read_text(encoding="utf-8"))
        if isinstance(data, dict) and isinstance(data.get("bundles"), dict):
            return data["bundles"]
    except Exception:
        pass
    return {}


def save_bundle_cache(bundles: dict) -> None:
    """Persist the cache. Atomic via tmp file rename."""
    _CACHE_PATH.parent.mkdir(parents=True, exist_ok=True)
    payload = {"schema": 1, "bundles": bundles}
    tmp = _CACHE_PATH.with_suffix(".tmp")
    tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    tmp.replace(_CACHE_PATH)
