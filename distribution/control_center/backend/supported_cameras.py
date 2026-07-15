"""Supported-camera list snapshot committed into the docs repo.

The release center mirrors the camera_db of the published niyien-lens-data
tag into `cameras/cameras.json` inside the docs repository (the niyien.com
Vercel deployment), so the /cameras/ page always matches the data that
clients actually receive (change: control-center-supported-cameras).

Pure payload helpers are separated from the GitHub I/O so tests cover the
mapping/format semantics without network:

- brand order and display names live here (moved from the retired
  docs/scripts/generate_supported_cameras.py; camera_db itself stays free
  of display-only fields)
- vendor files and the BRANDS registry are cross-checked both ways so a new
  vendor JSON can't silently miss the page
- change detection ignores `generated_at`, so a publish with unchanged
  camera_db content produces no docs commit
"""

from __future__ import annotations

import base64
import datetime
import json

# Brand order defines the display order on the /cameras/ page.
# (file stem, display name) — every camera_db/*.json must be listed here.
BRANDS: list[tuple[str, str]] = [
    ("sony", "Sony"),
    ("canon", "Canon"),
    ("nikon", "Nikon"),
    ("fujifilm", "Fujifilm"),
    ("lumix", "Panasonic LUMIX"),
    ("blackmagic", "Blackmagic Design"),
    ("red", "RED"),
    ("kinefinity", "Kinefinity"),
    ("leica", "Leica"),
    ("sigma", "Sigma"),
    ("ricoh", "Ricoh"),
    ("zcam", "Z CAM"),
]

# camera_db key -> market display name. Unlisted keys pass through as-is.
DISPLAY_NAMES: dict[str, dict[str, str]] = {
    "sony": {
        "ILCE-1M2":  "α1 II",
        "ILCE-1":    "α1",
        "ILCE-9M3":  "α9 III",
        "ILCE-9M2":  "α9 II",
        "ILCE-9":    "α9",
        "ILCE-7SM3": "α7S III",
        "ILCE-7SM2": "α7S II",
        "ILCE-7S":   "α7S",
        "ILCE-7RM5": "α7R V",
        "ILCE-7RM4": "α7R IV",
        "ILCE-7RM3": "α7R III",
        "ILCE-7RM2": "α7R II",
        "ILCE-7M4":  "α7 IV",
        "ILCE-7M3":  "α7 III",
        "ILCE-7M2":  "α7 II",
        "ILCE-7":    "α7",
        "ILCE-7CM2": "α7C II",
        "ILCE-7CR":  "α7CR",
        "ILCE-7C":   "α7C",
        "ILCE-FX2":  "FX2",
        "ILCE-FX30": "FX30",
        "ILCE-FX3":  "FX3",
        "ILCE-FX6":  "FX6",
        "ILCE-6700": "α6700",
        "ILCE-6600": "α6600",
        "ILCE-6500": "α6500",
        "ILCE-6400": "α6400",
        "ILCE-6300": "α6300",
        "ILCE-6100": "α6100",
        "ZVE1":      "ZV-E1",
        "ZVE10M2":   "ZV-E10 II",
        "ZVE10":     "ZV-E10",
        "ZV1":       "ZV-1",
        "RX100M7":   "RX100 VII",
        "RX100VA":   "RX100 VA",
        "RX1RM2":    "RX1R II",
        "RX10M4":    "RX10 IV",
    },
    "lumix": {
        "S1M2":    "S1 II",
        "S1R2":    "S1R II",
        "S5M2X":   "S5 IIX",
        "S5M2":    "S5 II",
        "G9M2":    "G9 II",
        "GH5M2":   "GH5 II",
        "LX100M2": "LX100 II",
    },
    "leica": {
        "240P": "M (Typ 240)",
    },
}


def build_cameras_payload(vendor_jsons: dict[str, dict], generated_at: str) -> dict:
    """Build the cameras.json document from parsed vendor JSONs.

    `vendor_jsons` maps file stem (e.g. "sony") to the parsed camera_db JSON.
    Raises ValueError when the stems and the BRANDS registry disagree in
    either direction, or when a registered vendor has an empty models section.
    """
    stems = set(vendor_jsons.keys())
    brand_stems = {stem for stem, _ in BRANDS}
    unregistered = sorted(stems - brand_stems)
    missing = sorted(brand_stems - stems)
    if unregistered or missing:
        parts = []
        if unregistered:
            parts.append(f"vendor file(s) not registered in BRANDS: {', '.join(unregistered)}")
        if missing:
            parts.append(f"BRANDS entry has no vendor file: {', '.join(missing)}")
        raise ValueError("; ".join(parts))

    brands_out = []
    for stem, display in BRANDS:
        models_obj = vendor_jsons[stem].get("models")
        keys = list(models_obj.keys()) if isinstance(models_obj, dict) else []
        if not keys:
            raise ValueError(f"{stem}.json has an empty models section")
        mapping = DISPLAY_NAMES.get(stem, {})
        brands_out.append(
            {
                "id": stem,
                "name": display,
                "models": [mapping.get(key, key) for key in keys],
            }
        )
    return {"generated_at": generated_at, "brands": brands_out}


def dumps_cameras(doc: dict) -> bytes:
    """Stable pretty JSON bytes, byte-identical format to the committed file."""
    return (json.dumps(doc, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def payload_equivalent(a: dict | None, b: dict | None) -> bool:
    """Content comparison that ignores `generated_at`.

    Without this every publish would commit a date-only change to docs.
    """
    a_brands = a.get("brands") if isinstance(a, dict) else None
    b_brands = b.get("brands") if isinstance(b, dict) else None
    return a_brands == b_brands


def _decode_contents_payload(payload: dict) -> bytes:
    content_b64 = str(payload.get("content", "") or "")
    return base64.b64decode(content_b64)


def fetch_camera_db(github_client, *, owner: str, repo: str, ref: str) -> dict[str, dict]:
    """Read camera_db/*.json from the lens-data repo at `ref` via contents API.

    Returns {stem: parsed JSON}. Raises on any listing/read/parse failure —
    the caller degrades to a warning; a partial vendor set must never be
    written out as a complete list.
    """
    listing = github_client.get_contents(owner, repo, path="camera_db", ref=ref)
    if not isinstance(listing, list):
        raise RuntimeError(f"camera_db directory not found in {owner}/{repo}@{ref}")
    vendor_jsons: dict[str, dict] = {}
    for item in listing:
        name = str(item.get("name", "") or "")
        if item.get("type") != "file" or not name.endswith(".json"):
            continue
        payload = github_client.get_contents(owner, repo, path=f"camera_db/{name}", ref=ref)
        if payload is None:
            raise RuntimeError(f"camera_db/{name} disappeared while reading {owner}/{repo}@{ref}")
        vendor_jsons[name[:-5]] = json.loads(_decode_contents_payload(payload).decode("utf-8"))
    if not vendor_jsons:
        raise RuntimeError(f"camera_db in {owner}/{repo}@{ref} contains no vendor JSON")
    return vendor_jsons


def sync_supported_cameras(
    lens_client,
    docs_client,
    *,
    lens_owner: str,
    lens_repo: str,
    lens_ref: str,
    docs_owner: str,
    docs_repo: str,
    docs_branch: str,
    docs_path: str,
    commit_message: str,
    now_date: str | None = None,
) -> dict:
    """Build the list from the published tag and GET-compare-PUT the docs file.

    No-op (no commit) when the brands content is unchanged. A stale-sha
    conflict is retried once with a fresh read. Returns
    {changed, brands, models, tag}. Raises on unrecoverable errors — the
    publish caller degrades to a warning, the manual caller surfaces it.
    """
    from .github import ContentsConflictError

    today = now_date or datetime.date.today().isoformat()
    vendor_jsons = fetch_camera_db(lens_client, owner=lens_owner, repo=lens_repo, ref=lens_ref)
    new_doc = build_cameras_payload(vendor_jsons, today)
    counts = {
        "brands": len(new_doc["brands"]),
        "models": sum(len(b["models"]) for b in new_doc["brands"]),
        "tag": lens_ref,
    }

    last_error: Exception | None = None
    for _attempt in range(2):
        existing_payload = docs_client.get_contents(
            docs_owner, docs_repo, path=docs_path, ref=docs_branch
        )
        sha = ""
        existing_doc: dict | None = None
        if existing_payload is not None:
            sha = str(existing_payload.get("sha", "") or "")
            try:
                existing_doc = json.loads(_decode_contents_payload(existing_payload).decode("utf-8"))
            except Exception:
                existing_doc = None  # unreadable current file: overwrite it
        if existing_doc is not None and payload_equivalent(existing_doc, new_doc):
            return {"changed": False, **counts}
        try:
            docs_client.put_contents(
                docs_owner,
                docs_repo,
                path=docs_path,
                message=commit_message,
                content_bytes=dumps_cameras(new_doc),
                sha=sha,
                branch=docs_branch,
            )
            return {"changed": True, **counts}
        except ContentsConflictError as e:
            last_error = e
            continue
    raise RuntimeError(f"supported cameras write failed after retry: {last_error}")
