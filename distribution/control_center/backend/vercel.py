"""Vercel REST API client — ported from legacy control_center.py::VercelClient."""

from __future__ import annotations

import json

import requests

from .helpers import build_proxy_mapping, normalize_proxy_url


class VercelClient:
    def __init__(self, token: str, project: str, team_id: str = "", proxy_url: str = ""):
        self.token = token.strip()
        self.project = project.strip()
        self.team_id = team_id.strip()
        self.proxy_url = normalize_proxy_url(proxy_url)

    def _params(self) -> dict:
        return {"teamId": self.team_id} if self.team_id else {}

    def _headers(self) -> dict:
        return {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"}

    def _request_kwargs(self, *, timeout: int, **kwargs) -> dict:
        payload = dict(kwargs)
        payload["timeout"] = timeout
        proxies = build_proxy_mapping(self.proxy_url)
        if proxies:
            payload["proxies"] = proxies
        return payload

    def _ensure_ready(self) -> None:
        if not self.token or not self.project:
            raise RuntimeError("Missing Vercel token or project id/name")

    def list_env_records(self) -> dict:
        """Return mapping key -> full env dict. Production target preferred."""
        self._ensure_ready()
        url = f"https://api.vercel.com/v10/projects/{self.project}/env"
        response = requests.get(
            url,
            headers=self._headers(),
            params={**self._params(), "decrypt": "true"},
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        payload = response.json()
        envs = payload.get("envs") if isinstance(payload, dict) else payload
        result: dict = {}
        for env in envs or []:
            key = env.get("key")
            target = env.get("target") or []
            if isinstance(target, str):
                target = [target]
            if key and "production" in target:
                result[key] = dict(env)
            elif key and key not in result:
                result[key] = dict(env)
        return result

    def list_envs(self) -> dict:
        """Return mapping key -> value (decrypted)."""
        return {k: env.get("value", "") for k, env in self.list_env_records().items()}

    @staticmethod
    def _looks_encrypted(value: str) -> bool:
        """Detect Vercel encrypted envelope wrapper.

        The list endpoint returns encrypted values either as the raw envelope
        JSON `{"v":"v2","c":"..."}` or as its base64 form (starts with `eyJ`,
        decoded = `{"`). Both are non-meaningful to the dashboard.
        """
        s = (value or "").strip()
        if not s:
            return False
        # Base64-encoded envelope: "eyJ2Ijoi" decodes to '{"v":"'
        if s.startswith("eyJ"):
            return True
        # Raw envelope JSON
        if s.startswith('{"v":"v') and '"c":"' in s:
            return True
        return False

    def list_envs_decrypted(self) -> dict:
        """Return {key: plain_value} — transparently fall back to single-env
        endpoint for `type=encrypted` records that the list endpoint left
        wrapped. Single-env calls run in a small ThreadPool so dashboard
        load doesn't pay 6-7x the RTT.
        """
        from concurrent.futures import ThreadPoolExecutor

        records = self.list_env_records()

        def _resolve(item):
            key, rec = item
            val = str(rec.get("value", ""))
            rec_type = str(rec.get("type", "")).lower()
            if rec_type == "encrypted" or self._looks_encrypted(val):
                env_id = str(rec.get("id", "")).strip()
                if env_id:
                    try:
                        val = self.get_env_value(env_id)
                    except Exception:
                        pass  # leave wrapped; caller decides
            return key, val

        items = list(records.items())
        if not items:
            return {}
        with ThreadPoolExecutor(max_workers=min(8, len(items))) as pool:
            return dict(pool.map(_resolve, items))

    def get_env_value(self, env_id: str) -> str:
        """Single-env endpoint returns the decrypted value (unlike the list
        endpoint which keeps `encrypted` type values wrapped as
        `{"v":"v2","c":"..."}` even with ?decrypt=true).
        """
        self._ensure_ready()
        if not env_id:
            raise RuntimeError("env_id is required")
        url = f"https://api.vercel.com/v9/projects/{self.project}/env/{env_id}"
        response = requests.get(
            url,
            headers=self._headers(),
            params={**self._params(), "decrypt": "true"},
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        payload = response.json()
        return str(payload.get("value", ""))

    def upsert_envs(self, mapping: dict) -> dict:
        self._ensure_ready()
        url = f"https://api.vercel.com/v10/projects/{self.project}/env"
        body = [
            {
                "key": key,
                "value": value,
                "type": "encrypted",
                "target": ["production", "preview", "development"],
            }
            for key, value in mapping.items()
        ]
        response = requests.post(
            url,
            headers=self._headers(),
            params={**self._params(), "upsert": "true"},
            json=body,
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        return response.json()

    def latest_production_deployment(self) -> dict | None:
        # Vercel runtime env vars are bound to a deployment; mutating envs
        # alone does not propagate to the running production. We need the
        # most recent READY production deployment so we can clone its
        # gitSource into a new one — the only way to make upserted envs
        # take effect when no deploy hook is configured.
        self._ensure_ready()
        url = "https://api.vercel.com/v6/deployments"
        params = {
            **self._params(),
            "projectId": self.project,
            "target": "production",
            "state": "READY",
            "limit": 1,
        }
        response = requests.get(
            url,
            headers=self._headers(),
            params=params,
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        deployments = response.json().get("deployments") or []
        return deployments[0] if deployments else None

    def redeploy_production(self) -> dict:
        latest = self.latest_production_deployment()
        if not latest:
            raise RuntimeError("Vercel: no READY production deployment found to clone")
        meta = latest.get("meta") or {}
        if meta.get("githubRepoId"):
            git_source = {
                "type": "github",
                "repoId": str(meta["githubRepoId"]),
                "ref": meta.get("githubCommitRef") or "main",
            }
        elif meta.get("gitlabProjectId"):
            git_source = {
                "type": "gitlab",
                "projectId": str(meta["gitlabProjectId"]),
                "ref": meta.get("gitlabCommitRef") or "main",
            }
        elif meta.get("bitbucketRepoUuid"):
            git_source = {
                "type": "bitbucket",
                "repoUuid": str(meta["bitbucketRepoUuid"]),
                "ref": meta.get("bitbucketCommitRef") or "main",
            }
        else:
            raise RuntimeError(
                f"Vercel: cannot derive gitSource from latest deployment meta: {meta}"
            )
        url = "https://api.vercel.com/v13/deployments"
        body = {
            "name": latest.get("name") or self.project,
            "target": "production",
            "gitSource": git_source,
        }
        response = requests.post(
            url,
            headers=self._headers(),
            params=self._params(),
            json=body,
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        return response.json()


def parse_policy_from_envs(envs: dict) -> dict:
    """Decode NIYIEN_RELEASE_POLICY_JSON into a dict.

    Returns either a valid policy, or a sentinel dict signalling that the
    value came back undecryptable (so the UI can tell the user to fix the
    token scope instead of silently showing an empty policy).
    """
    raw = str(envs.get("NIYIEN_RELEASE_POLICY_JSON", "")).strip()
    default = {
        "versions": [],
        "auto_version": "",
        "default_manifest_ttl": 300,
    }
    if not raw:
        return default
    # Values encrypted by Vercel (or application-level) are not plain JSON.
    # Detect the common `{"v":"v2","c":"..."}` envelope (base64 => starts with eyJ2)
    # and any other non-JSON prefix → flag explicitly.
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return {**default, "encrypted": True, "raw_length": len(raw)}
    if not isinstance(parsed, dict):
        return default
    # Recognize Vercel's encrypted wrapper even if it parsed as JSON
    if "versions" not in parsed and set(parsed.keys()).issubset({"v", "c", "i", "s", "t"}):
        return {**default, "encrypted": True, "raw_length": len(raw)}
    if not isinstance(parsed.get("versions"), list):
        return default
    return parsed
