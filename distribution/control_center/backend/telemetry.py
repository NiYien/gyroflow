"""Telemetry stats + rebuild endpoints (niyien.com server)."""

from __future__ import annotations

import requests

from .helpers import build_proxy_mapping

DEFAULT_PRODUCT_ID = "gyroflow_niyien"


def fetch_stats(base_url: str, token: str, days: int, event: str = "", proxy_url: str = "") -> dict:
    base = base_url.rstrip("/")
    if not base:
        raise RuntimeError("telemetry_base_url not configured")
    params: dict = {"days": str(max(1, int(days))), "product_id": DEFAULT_PRODUCT_ID}
    if event:
        params["event"] = event
    headers: dict = {}
    if token:
        headers["X-Stats-Token"] = token
    proxies = build_proxy_mapping(proxy_url)
    response = requests.get(
        f"{base}/api/telemetry-stats",
        params=params,
        headers=headers,
        timeout=30,
        proxies=proxies,
    )
    response.raise_for_status()
    return response.json()


def trigger_rebuild(base_url: str, token: str, start_day: str, end_day: str, proxy_url: str = "") -> dict:
    base = base_url.rstrip("/")
    if not base or not token:
        raise RuntimeError("telemetry_base_url or telemetry_rebuild_token missing")
    payload = {
        "start_day": start_day.strip(),
        "end_day": end_day.strip(),
        "dry_run": False,
        "apply": True,
        "reset_day_keys": False,
    }
    proxies = build_proxy_mapping(proxy_url)
    response = requests.post(
        f"{base}/api/telemetry-rebuild",
        headers={"X-Rebuild-Token": token, "Content-Type": "application/json"},
        json=payload,
        timeout=60,
        proxies=proxies,
    )
    response.raise_for_status()
    return response.json()


def fetch_manifest(base_url: str, country: str, platform: str, proxy_url: str = "") -> dict:
    """Hit the public manifest endpoint to preview what clients see."""
    base = base_url.rstrip("/")
    if not base:
        raise RuntimeError("telemetry_base_url not configured")
    proxies = build_proxy_mapping(proxy_url)
    response = requests.get(
        f"{base}/api/manifest",
        params={"country": country.upper(), "platform": platform.lower()},
        timeout=30,
        proxies=proxies,
    )
    response.raise_for_status()
    return response.json()
