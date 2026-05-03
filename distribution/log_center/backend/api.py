"""niyien.com admin API client — wraps GET /api/feedback/list.

Feedback admin token comes from log_center.config.json. The list endpoint
is documented in `docs/feedback-schema.md` §1 and the OpenSpec change
`feedback-server-endpoints` (Phase 3).
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from typing import Optional

import requests


class AuthError(RuntimeError):
    """401 from /api/feedback/list — token rejected."""


class ServerError(RuntimeError):
    """5xx from /api/feedback/list — surfaces a body excerpt."""


class NiyenApiClient:
    """Tiny client for the admin endpoints.

    Currently exposes only ``list_feedback``; extend as Phase 3 grows
    (e.g. a future ``DELETE /api/feedback/<id>`` would land here).
    """

    def __init__(self, base_url: str, admin_token: str, *, timeout: int = 30):
        self.base_url = base_url.rstrip("/")
        self.admin_token = admin_token
        self.timeout = int(timeout)
        self._session = requests.Session()
        # Bypass any system HTTP_PROXY / HTTPS_PROXY — niyien admin tool runs
        # locally and must hit niyien.com directly (a proxy could silently
        # intercept the bearer token).
        self._session.trust_env = False

    def _headers(self) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {self.admin_token}",
            "Accept": "application/json",
        }

    def list_feedback(
        self,
        *,
        since: Optional[datetime] = None,
        limit: int = 500,
    ) -> list[dict]:
        """Fetch records newer than ``since`` (default = 30 days ago).

        Returns the ``items`` array; each item has the shape documented in
        feedback-schema.md (id, region, ts, app_version, os, gpu, summary,
        email, size, sha256, bucket_path, ip_hash, confirmed_ts).
        """
        if since is None:
            since = datetime.now(timezone.utc) - timedelta(days=30)
        params = {
            "since": since.astimezone(timezone.utc).isoformat().replace("+00:00", "Z"),
            "limit": int(limit),
        }
        url = f"{self.base_url}/feedback/list"
        try:
            r = self._session.get(
                url, params=params, headers=self._headers(), timeout=self.timeout
            )
        except requests.RequestException as exc:
            raise ServerError(f"network error contacting {url}: {exc}") from exc
        if r.status_code == 401:
            raise AuthError("Admin token rejected by /api/feedback/list (401)")
        if r.status_code >= 500:
            excerpt = r.text[:300].replace("\n", " ")
            raise ServerError(f"server returned {r.status_code}: {excerpt!r}")
        if r.status_code >= 400:
            excerpt = r.text[:300].replace("\n", " ")
            raise ServerError(f"client error {r.status_code}: {excerpt!r}")
        try:
            payload = r.json()
        except ValueError as exc:
            excerpt = r.text[:300].replace("\n", " ")
            raise ServerError(f"non-JSON response: {excerpt!r}") from exc
        items = payload.get("items") if isinstance(payload, dict) else None
        if not isinstance(items, list):
            raise ServerError(f"unexpected payload shape: {payload!r}")
        return items
