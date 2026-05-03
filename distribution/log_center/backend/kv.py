"""Upstash Redis REST client (HTTPS, not redis TCP) — see design D10.

Only the operations log_center needs are implemented: GET, DEL, LREM.
The Upstash REST API accepts the command + args as URL path segments and
returns ``{"result": ...}`` JSON.
"""

from __future__ import annotations

from typing import Optional
from urllib.parse import quote

import requests


class UpstashKvError(RuntimeError):
    """Generic Upstash REST API failure."""


class UpstashKvClient:
    def __init__(self, rest_url: str, rest_token: str, *, timeout: int = 15):
        self.base_url = rest_url.rstrip("/")
        self.token = rest_token
        self.timeout = int(timeout)
        self._session = requests.Session()
        # Bypass any system HTTP_PROXY / HTTPS_PROXY — Upstash REST traffic
        # carries the KV bearer token; a transparent proxy could log it.
        self._session.trust_env = False

    def _headers(self) -> dict[str, str]:
        return {"Authorization": f"Bearer {self.token}"}

    @staticmethod
    def _seg(value: str) -> str:
        # quote(value, safe="") encodes ":" as "%3A" which Upstash accepts.
        return quote(value, safe="")

    def _request(self, segments: list[str]) -> object:
        url = self.base_url + "/" + "/".join(self._seg(s) for s in segments)
        try:
            r = self._session.get(url, headers=self._headers(), timeout=self.timeout)
        except requests.RequestException as exc:
            raise UpstashKvError(f"network error: {exc}") from exc
        if r.status_code >= 400:
            excerpt = r.text[:200].replace("\n", " ")
            raise UpstashKvError(f"HTTP {r.status_code}: {excerpt!r}")
        try:
            payload = r.json()
        except ValueError as exc:
            raise UpstashKvError(f"non-JSON response: {r.text[:200]!r}") from exc
        if "error" in payload:
            raise UpstashKvError(str(payload["error"]))
        return payload.get("result")

    # ---- ops ----

    def get(self, key: str) -> Optional[str]:
        result = self._request(["get", key])
        if result is None:
            return None
        return str(result)

    def delete(self, key: str) -> int:
        """DEL key. Returns 1 if removed, 0 if missing."""
        result = self._request(["del", key])
        try:
            return int(result or 0)
        except (TypeError, ValueError):
            return 0

    def lrem(self, list_key: str, value: str, *, count: int = 0) -> int:
        """LREM <list_key> <count> <value>. Default count=0 removes all
        matches (matches the design D5 description).
        """
        result = self._request(["lrem", list_key, str(int(count)), value])
        try:
            return int(result or 0)
        except (TypeError, ValueError):
            return 0
