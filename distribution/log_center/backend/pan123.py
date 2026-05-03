"""Standalone 123 OpenAPI client for log_center — download + delete only.

Independent of ``distribution/control_center/backend/pan123.py`` (no import,
no shared module) per design D2. The list/upload helpers from the publish
script are intentionally absent here; we only need to:

  * resolve a bucket-relative path like ``feedback/20260502/<id>.zip``
    into a numeric fileID, and
  * download or trash that file via the OpenAPI.

Token caching: the OpenAPI access token is valid ~30 minutes; we refresh
on expiry (60 s safety margin). All traffic bypasses the local network
proxy — 123 servers are reachable directly inside CN networks and routing
through a GFW-bypass proxy hurts throughput badly (mirrors the rationale
in control_center/backend/pan123.py).

Reference: https://www.123pan.com/developer
"""

from __future__ import annotations

import time
from datetime import datetime
from pathlib import Path
from typing import Optional

import requests

DEFAULT_API_BASE = "https://open-api.123pan.com"
PLATFORM_HEADER = "open_platform"


class Pan123Error(RuntimeError):
    """Any non-zero ``code`` from the 123 OpenAPI."""


class Pan123Client:
    """Minimal 123 OpenAPI client. Caches access token in-memory."""

    def __init__(
        self,
        client_id: str,
        client_secret: str,
        feedback_root: str = "/feedback",
        *,
        api_base: str = DEFAULT_API_BASE,
        timeout: int = 60,
    ):
        self.client_id = (client_id or "").strip()
        self.client_secret = (client_secret or "").strip()
        # Stored as a "/" prefixed absolute virtual path inside 123 root.
        self.feedback_root = "/" + feedback_root.strip("/") if feedback_root else "/"
        self.api_base = api_base.rstrip("/")
        self.timeout = int(timeout)
        self._token: Optional[str] = None
        self._token_exp: float = 0.0
        self._session = requests.Session()
        # Bypass any user-configured network proxy — see module docstring.
        self._session.trust_env = False

    # ---------------- token ----------------

    def _request_kwargs(self, *, timeout: Optional[int] = None) -> dict:
        return {
            "timeout": int(timeout if timeout is not None else self.timeout),
            "proxies": {"http": None, "https": None},
        }

    def get_access_token(self) -> str:
        if self._token and time.time() < self._token_exp - 60:
            return self._token
        if not self.client_id or not self.client_secret:
            raise Pan123Error("missing pan123 client_id / client_secret")
        url = f"{self.api_base}/api/v1/access_token"
        body = {"clientID": self.client_id, "clientSecret": self.client_secret}
        r = self._session.post(
            url,
            json=body,
            headers={"Platform": PLATFORM_HEADER, "Content-Type": "application/json"},
            **self._request_kwargs(timeout=30),
        )
        r.raise_for_status()
        payload = r.json()
        if int(payload.get("code", -1)) != 0:
            raise Pan123Error(f"auth failed: {payload.get('message') or payload}")
        data = payload.get("data") or {}
        token = str(data.get("accessToken", "")).strip()
        if not token:
            raise Pan123Error(f"auth response missing accessToken: {payload}")
        self._token = token
        # ``expiredAt`` is ISO-ish; fall back to 30 min if parsing fails.
        expired_at = str(data.get("expiredAt") or "")
        try:
            self._token_exp = datetime.fromisoformat(
                expired_at.replace("Z", "+00:00")
            ).timestamp()
        except Exception:
            self._token_exp = time.time() + 1800
        return self._token

    def _headers(self) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {self.get_access_token()}",
            "Platform": PLATFORM_HEADER,
            "Content-Type": "application/json",
        }

    # ---------------- low-level request ----------------

    def _get(self, path: str, params: Optional[dict] = None) -> dict:
        r = self._session.get(
            f"{self.api_base}{path}",
            params=params,
            headers=self._headers(),
            **self._request_kwargs(),
        )
        r.raise_for_status()
        payload = r.json()
        if int(payload.get("code", -1)) != 0:
            raise Pan123Error(f"GET {path}: {payload.get('message') or payload}")
        return payload.get("data") or {}

    def _post(self, path: str, body: Optional[dict] = None) -> dict:
        r = self._session.post(
            f"{self.api_base}{path}",
            json=body or {},
            headers=self._headers(),
            **self._request_kwargs(),
        )
        r.raise_for_status()
        payload = r.json()
        if int(payload.get("code", -1)) != 0:
            raise Pan123Error(f"POST {path}: {payload.get('message') or payload}")
        return payload.get("data") or {}

    # ---------------- directory traversal ----------------

    def _list_directory(self, parent_id: int, *, page_size: int = 100) -> list[dict]:
        """Walk every page and return all entries. Stops at first empty page
        or when ``lastFileId`` returns ``-1``.
        """
        out: list[dict] = []
        seen: set[int] = set()
        last_file_id = 0
        for _ in range(50):
            data = self._get(
                "/api/v2/file/list",
                params={
                    "parentFileId": int(parent_id),
                    "limit": int(page_size),
                    "lastFileId": int(last_file_id),
                },
            )
            entries = data.get("fileList") or []
            new_in_page = 0
            for entry in entries:
                fid = int(entry.get("fileID") or entry.get("fileId") or 0)
                if fid and fid in seen:
                    continue
                if fid:
                    seen.add(fid)
                out.append(entry)
                new_in_page += 1
            next_last = int(data.get("lastFileId", -1) or -1)
            if next_last == -1 or not entries or new_in_page == 0:
                break
            if next_last == last_file_id:
                break
            last_file_id = next_last
        return out

    def _find_child(self, parent_id: int, name: str, *, is_dir: bool) -> Optional[dict]:
        wanted_type = 1 if is_dir else 0
        for entry in self._list_directory(parent_id):
            ename = str(entry.get("filename") or entry.get("name") or "").strip()
            if ename != name:
                continue
            if int(entry.get("type", -1)) != wanted_type:
                continue
            if int(entry.get("trashed", 0)) != 0:
                continue
            return entry
        return None

    def resolve_path_to_file_id(self, path: str) -> Optional[int]:
        """Walk a virtual path like ``/feedback/20260502/<id>.zip`` from
        the 123 root and return the leaf fileID (or None if any segment
        is missing).
        """
        segs = [s for s in path.replace("\\", "/").split("/") if s]
        parent_id = 0  # 0 == root
        last_index = len(segs) - 1
        for i, seg in enumerate(segs):
            is_last = i == last_index
            child = self._find_child(parent_id, seg, is_dir=not is_last)
            if not child:
                return None
            parent_id = int(child.get("fileID") or child.get("fileId") or 0)
            if not parent_id:
                return None
        return parent_id or None

    # ---------------- public ops ----------------

    def normalize_path(self, bucket_path: str) -> str:
        """Ensure a leading ``/`` and that ``feedback_root`` is at the head.

        Server stores ``bucket_path`` as either:
          (a) ``feedback/20260502/<id>.zip`` — relative to 123 root, or
          (b) ``/feedback/20260502/<id>.zip`` — absolute virtual path.

        Both end up producing the same absolute path here.
        """
        p = "/" + bucket_path.replace("\\", "/").lstrip("/")
        # If the path doesn't already start with feedback_root, prepend it.
        root = self.feedback_root.rstrip("/") + "/"
        if not (p == self.feedback_root or p.startswith(root)):
            p = root + p.lstrip("/")
        return p

    def get_download_url(self, file_id: int) -> str:
        if int(file_id or 0) <= 0:
            raise Pan123Error("get_download_url: invalid file_id")
        data = self._get("/api/v1/file/download_info", params={"fileId": int(file_id)})
        url = str(data.get("downloadUrl", "")).strip()
        if not url:
            raise Pan123Error(f"download_info: empty downloadUrl (fileId={file_id})")
        return url

    def download(self, bucket_path: str, target_path: Path) -> None:
        """Resolve path → fileID → downloadUrl, stream to ``target_path``."""
        abs_path = self.normalize_path(bucket_path)
        file_id = self.resolve_path_to_file_id(abs_path)
        if not file_id:
            raise Pan123Error(f"path not found in 123: {abs_path}")
        url = self.get_download_url(file_id)
        target_path.parent.mkdir(parents=True, exist_ok=True)
        # The download URL is signed and short-lived; do NOT pass auth headers.
        with self._session.get(
            url, stream=True, **self._request_kwargs(timeout=300)
        ) as r:
            r.raise_for_status()
            with target_path.open("wb") as fh:
                for chunk in r.iter_content(chunk_size=64 * 1024):
                    if chunk:
                        fh.write(chunk)

    def delete(self, bucket_path: str) -> None:
        """Move the file to the 123 recycle bin (the OpenAPI ``trash`` op).

        OpenAPI v1 endpoint: POST /api/v1/file/trash, body
        ``{"fileIDs": [<int>, ...]}``. We resolve the path first; if the
        file is already gone treat as success.
        """
        abs_path = self.normalize_path(bucket_path)
        file_id = self.resolve_path_to_file_id(abs_path)
        if not file_id:
            return  # already deleted; idempotent
        # Use file/trash to move into the recycle bin (matches the upstream
        # publish script which also avoids hard delete to keep an undo
        # window). Body shape per 123 OpenAPI v1 docs.
        self._post("/api/v1/file/trash", body={"fileIDs": [int(file_id)]})
