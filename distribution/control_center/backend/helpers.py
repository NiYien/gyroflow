"""Shared helper functions — no third-party imports beyond stdlib."""

from __future__ import annotations


SENSITIVE_KEYS = frozenset({
    "vercel_token",
    "github_token",
    "telemetry_stats_token",
    "telemetry_rebuild_token",
    "deploy_hook_url",
    "pan123_client_id",
    "pan123_client_secret",
    "pan123_releases_root_id",
})


def normalize_proxy_url(value: str) -> str:
    proxy = str(value or "").strip()
    if not proxy:
        return ""
    if "://" not in proxy:
        proxy = f"http://{proxy}"
    return proxy


def build_proxy_mapping(proxy_url: str) -> dict | None:
    proxy = normalize_proxy_url(proxy_url)
    if not proxy:
        return None
    return {"http": proxy, "https": proxy}


def normalize_version(tag: str) -> str:
    """Strip leading 'v' from a git tag."""
    return tag[1:] if tag.startswith("v") else tag


def mask_sensitive(cfg: dict) -> dict:
    """Return a copy with sensitive string values partially masked for UI display."""
    out = {}
    for k, v in cfg.items():
        if k in SENSITIVE_KEYS and isinstance(v, str) and v:
            out[k] = v[:4] + "****" + v[-2:] if len(v) > 8 else "****"
        else:
            out[k] = v
    return out
