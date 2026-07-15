"""Append-only multilingual changelog archive maintained in the docs repo.

The release center mirrors every release-notes-writing publish action into
`changelog/archive.json` inside the docs repository (the niyien.com Vercel
deployment), so the /changelog/ history page has a durable data source that
survives policy version retirement and the 64KB Vercel env var cap
(change: changelog-history-page-and-cumulative-notes).

Pure merge/upsert helpers are separated from the GitHub I/O so tests cover
the archive semantics without network:

- entries sorted by version descending, mirroring the client's
  `compare_app_versions` ordering (numeric base; bare base older than any
  suffixed build of the same base; ni > dev > other schemas; numeric
  sequence within a schema; unparseable versions sort oldest)
- upsert overwrites content fields but keeps a non-empty `published_at`
  (first-publish time survives re-publishes)
- self-heal backfill: policy versions missing from the archive are added
  with `published_at=""` so one failed write never leaves a permanent hole
"""

from __future__ import annotations

import datetime
import json

ARCHIVE_SCHEMA = 1
ARCHIVE_PRODUCT = "gyroflow-niyien"

# Higher = newer at the same base. Mirrors src/distribution.rs::schema_priority.
_SCHEMA_PRIORITY = {"ni": 2, "dev": 1}


def version_sort_key(version: str) -> tuple:
    """Ordering key mirroring the gyroflow client's compare_app_versions.

    Unparseable versions sort before (older than) any parseable one; among
    themselves they fall back to plain string ordering.
    """
    trimmed = str(version or "").strip().lstrip("v")
    base_str, _, suffix_raw = trimmed.partition("-")
    parts = base_str.split(".")
    if len(parts) == 3 and all(p.isdigit() for p in parts):
        base = tuple(int(p) for p in parts)
        if not suffix_raw:
            # Bare base is the FIRST release of that base.
            return (1, base, 0, 0, 0, "")
        schema, _, seq_raw = suffix_raw.partition(".")
        priority = _SCHEMA_PRIORITY.get(schema, 0)
        if seq_raw.isdigit():
            return (1, base, 1, priority, 1, int(seq_raw))
        return (1, base, 1, priority, 0, suffix_raw)
    return (0, (0, 0, 0), 0, 0, 0, trimmed)


def utc_now_iso() -> str:
    return (
        datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def entry_from_policy(policy_version: dict, published_at: str = "") -> dict | None:
    """Convert one policy.versions[] item into an archive entry.

    Returns None when the item has no usable version string.
    """
    if not isinstance(policy_version, dict):
        return None
    version = str(policy_version.get("version", "") or "").strip()
    if not version:
        return None
    raw_changelogs = policy_version.get("changelogs")
    changelogs: dict[str, str] = {}
    if isinstance(raw_changelogs, dict):
        for code, text in raw_changelogs.items():
            if isinstance(code, str) and isinstance(text, str) and text.strip():
                changelogs[code] = text
    return {
        "version": version,
        "tag": str(policy_version.get("tag", "") or ""),
        "published_at": published_at,
        "changelog": str(policy_version.get("changelog", "") or ""),
        "changelogs": changelogs,
        "recommended": bool(policy_version.get("recommended", False)),
    }


def empty_archive() -> dict:
    return {"schema": ARCHIVE_SCHEMA, "product": ARCHIVE_PRODUCT, "entries": []}


def normalize_archive(raw: dict | None) -> dict:
    """Coerce a loaded archive document into the canonical shape.

    Tolerates a missing/foreign document (None, wrong types) by starting
    fresh — the archive is append-only and self-heals from policy.
    """
    if not isinstance(raw, dict):
        return empty_archive()
    entries = raw.get("entries")
    clean: list[dict] = []
    seen: set[str] = set()
    if isinstance(entries, list):
        for item in entries:
            if not isinstance(item, dict):
                continue
            version = str(item.get("version", "") or "").strip()
            if not version or version in seen:
                continue
            seen.add(version)
            raw_changelogs = item.get("changelogs")
            changelogs = (
                {k: v for k, v in raw_changelogs.items() if isinstance(k, str) and isinstance(v, str)}
                if isinstance(raw_changelogs, dict)
                else {}
            )
            clean.append(
                {
                    "version": version,
                    "tag": str(item.get("tag", "") or ""),
                    "published_at": str(item.get("published_at", "") or ""),
                    "changelog": str(item.get("changelog", "") or ""),
                    "changelogs": changelogs,
                    "recommended": bool(item.get("recommended", False)),
                }
            )
    doc = empty_archive()
    doc["entries"] = clean
    return doc


def merge_archive(
    archive: dict,
    *,
    upsert_entries: list[dict] | None = None,
    backfill_entries: list[dict] | None = None,
    now_iso: str,
) -> dict:
    """Merge entries into a normalized archive and return the new document.

    upsert_entries: overwrite content fields (tag/changelog/changelogs/
    recommended) of an existing same-version entry but keep its non-empty
    `published_at`; brand-new entries get `published_at = now_iso`.

    backfill_entries: self-heal additions — only appended when the version
    is missing entirely (never overwrite), with their own `published_at`
    (empty string for policy backfill / migration imports).
    """
    doc = normalize_archive(archive)
    by_version = {e["version"]: e for e in doc["entries"]}
    for entry in upsert_entries or []:
        if not entry:
            continue
        existing = by_version.get(entry["version"])
        if existing is not None:
            kept_published = existing.get("published_at", "") or entry.get("published_at", "")
            existing.update(entry)
            existing["published_at"] = kept_published or now_iso
        else:
            fresh = dict(entry)
            if not fresh.get("published_at"):
                fresh["published_at"] = now_iso
            by_version[fresh["version"]] = fresh
    for entry in backfill_entries or []:
        if not entry:
            continue
        if entry["version"] not in by_version:
            by_version[entry["version"]] = dict(entry)
    doc["entries"] = sorted(
        by_version.values(), key=lambda e: version_sort_key(e["version"]), reverse=True
    )
    return doc


def backfill_from_policy(archive: dict, policy_versions: list) -> list[dict]:
    """Entries for policy versions missing from the archive (published_at='')."""
    doc = normalize_archive(archive)
    have = {e["version"] for e in doc["entries"]}
    result: list[dict] = []
    for item in policy_versions or []:
        entry = entry_from_policy(item)
        if entry is not None and entry["version"] not in have:
            result.append(entry)
    return result


def dumps_archive(doc: dict) -> bytes:
    """Stable pretty JSON bytes for the committed archive file."""
    return (json.dumps(doc, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def loads_archive(payload: bytes | str | None) -> dict:
    """Parse archive file bytes; any parse failure starts a fresh document."""
    if payload is None:
        return empty_archive()
    try:
        if isinstance(payload, bytes):
            payload = payload.decode("utf-8")
        return normalize_archive(json.loads(payload))
    except Exception:
        return empty_archive()


def sync_archive(
    github_client,
    *,
    owner: str,
    repo: str,
    branch: str,
    path: str,
    commit_message: str,
    policy_versions: list,
    upsert_versions: list | None = None,
    now_iso: str | None = None,
) -> dict:
    """GET-merge-PUT the archive file in the docs repo (one conflict retry).

    `upsert_versions` are policy.versions[] items to upsert (the just
    published version); all other policy versions missing from the archive
    are backfilled with empty published_at. Returns
    {"added": N, "updated": M, "backfilled": K}.

    Raises on unrecoverable API errors — the caller degrades to a warning,
    publishing itself must never be blocked by the archive.
    """
    from .github import ContentsConflictError

    now = now_iso or utc_now_iso()
    upserts = [e for e in (entry_from_policy(v) for v in (upsert_versions or [])) if e]

    last_error: Exception | None = None
    for _attempt in range(2):
        import base64

        payload = github_client.get_contents(owner, repo, path=path, ref=branch)
        if payload is None:
            archive = empty_archive()
            sha = ""
        else:
            content_b64 = str(payload.get("content", "") or "")
            try:
                raw = base64.b64decode(content_b64)
            except Exception:
                raw = b""
            archive = loads_archive(raw)
            sha = str(payload.get("sha", "") or "")

        before = {e["version"]: e for e in normalize_archive(archive)["entries"]}
        upsert_set = {e["version"] for e in upserts}
        backfills = [
            b for b in backfill_from_policy(archive, policy_versions)
            if b["version"] not in upsert_set
        ]
        merged = merge_archive(
            archive, upsert_entries=upserts, backfill_entries=backfills, now_iso=now
        )
        after = {e["version"]: e for e in merged["entries"]}
        if after == before:
            return {"added": 0, "updated": 0, "backfilled": 0}

        added = sum(1 for e in upserts if e["version"] not in before)
        updated = sum(
            1
            for e in upserts
            if e["version"] in before and after[e["version"]] != before[e["version"]]
        )
        try:
            github_client.put_contents(
                owner,
                repo,
                path=path,
                message=commit_message,
                content_bytes=dumps_archive(merged),
                sha=sha,
                branch=branch,
            )
            return {"added": added, "updated": updated, "backfilled": len(backfills)}
        except ContentsConflictError as e:
            last_error = e
            continue
    raise RuntimeError(f"changelog archive write failed after retry: {last_error}")
