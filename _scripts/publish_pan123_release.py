#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import tarfile
import time
import zipfile
from urllib.parse import quote, urlparse
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable

import requests


APP_ASSET_NAMES = (
    "gyroflow-niyien-windows64-setup.exe",
    "gyroflow-niyien-windows64.zip",
    "gyroflow-niyien-mac-universal.dmg",
    "gyroflow-niyien-linux64.AppImage",
    "gyroflow-niyien.apk",
)
APP_ASSET_PLATFORM_BY_NAME = {
    "gyroflow-niyien-windows64-setup.exe": "windows",
    "gyroflow-niyien-windows64.zip": "windows",
    "gyroflow-niyien-mac-universal.dmg": "macos",
    "gyroflow-niyien-linux64.AppImage": "linux",
    "gyroflow-niyien.apk": "android",
}
APP_ASSET_ROLE_BY_NAME = {
    "gyroflow-niyien-windows64-setup.exe": "installer",
    "gyroflow-niyien-windows64.zip": "package",
    "gyroflow-niyien-mac-universal.dmg": "package",
    "gyroflow-niyien-linux64.AppImage": "package",
    "gyroflow-niyien.apk": "package",
}
APP_ASSET_NAMES_BY_PLATFORM = {
    "windows": (
        "gyroflow-niyien-windows64-setup.exe",
        "gyroflow-niyien-windows64.zip",
    ),
    "macos": ("gyroflow-niyien-mac-universal.dmg",),
    "linux": ("gyroflow-niyien-linux64.AppImage",),
    "android": ("gyroflow-niyien.apk",),
}

# Workflow_dispatch (nightly) mode uploads artifacts with short names that omit
# the file extension and platform-detail suffix, mirroring the gyroflow upstream
# layout. Each short artifact wraps exactly one raw deliverable (the
# extension-bearing filename). nightly.link serves the wrapper at
# `<short_name>.zip`; publish/client extracts one level to recover the raw file.
APP_ARTIFACT_NAMES_BY_FILE = {
    "gyroflow-niyien-windows":   "gyroflow-niyien-windows64.zip",
    "gyroflow-niyien-win-setup": "gyroflow-niyien-windows64-setup.exe",
    "gyroflow-niyien-mac":       "gyroflow-niyien-mac-universal.dmg",
}
APP_FILE_TO_ARTIFACT_NAME = {v: k for k, v in APP_ARTIFACT_NAMES_BY_FILE.items()}


def derive_required_app_asset_names(
    *,
    workflow_text: str | None = None,
    workflow_path: Path | None = None,
) -> tuple[str, ...]:
    if workflow_text is None:
        path = workflow_path or Path(__file__).resolve().parents[1] / ".github" / "workflows" / "release.yml"
        try:
            workflow_text = path.read_text(encoding="utf-8")
        except OSError:
            return tuple(APP_ASSET_NAMES)

    active_platforms: list[str] = []
    for match in re.finditer(r"\btype:\s*([A-Za-z0-9_-]+)", workflow_text):
        platform = match.group(1).strip().lower()
        if platform in APP_ASSET_NAMES_BY_PLATFORM and platform not in active_platforms:
            active_platforms.append(platform)

    if not active_platforms:
        return tuple(APP_ASSET_NAMES)

    required: list[str] = []
    for platform in ("windows", "macos", "linux", "android"):
        if platform in active_platforms:
            required.extend(APP_ASSET_NAMES_BY_PLATFORM[platform])
    return tuple(name for name in required if name in APP_ASSET_NAMES)


REQUIRED_APP_ASSET_NAMES = derive_required_app_asset_names()

PLUGIN_ASSET_NAMES = (
    "GyroflowNiyien-OpenFX-windows.zip",
    "GyroflowNiyien-Adobe-windows.aex",
    "GyroflowNiyien-OpenFX-macos.zip",
    "GyroflowNiyien-Adobe-macos.zip",
)

SDK_FILENAMES = (
    "Blackmagic_RAW_SDK_Windows_5.0.0.tar.gz",
    "Blackmagic_RAW_SDK_MacOS_5.0.0.tar.gz",
    "Blackmagic_RAW_SDK_Linux_5.0.0.tar.gz",
    "RED_SDK_Windows_9.1.2.tar.gz",
    "RED_SDK_MacOS_9.1.2.tar.gz",
    "RED_SDK_Linux_9.1.2.tar.gz",
)

SDK_DOWNLOAD_SOURCES = {
    "Blackmagic_RAW_SDK_Windows_5.0.0.tar.gz": (
        {"kind": "direct", "path": "Blackmagic_RAW_SDK_Windows_5.0.0.tar.gz"},
        {"kind": "direct", "path": "Blackmagic_RAW_SDK_Windows.tar.gz"},
    ),
    "Blackmagic_RAW_SDK_MacOS_5.0.0.tar.gz": (
        {"kind": "direct", "path": "Blackmagic_RAW_SDK_MacOS_5.0.0.tar.gz"},
        {"kind": "direct", "path": "Blackmagic_RAW_SDK_MacOS.tar.gz"},
    ),
    "Blackmagic_RAW_SDK_Linux_5.0.0.tar.gz": (
        {"kind": "direct", "path": "Blackmagic_RAW_SDK_Linux_5.0.0.tar.gz"},
        {"kind": "direct", "path": "Blackmagic_RAW_SDK_Linux.tar.gz"},
    ),
    "RED_SDK_Windows_9.1.2.tar.gz": (
        {"kind": "direct", "path": "RED_SDK_Windows_9.1.2.tar.gz"},
        {"kind": "direct", "path": "RED_SDK_Windows.tar.gz"},
    ),
    "RED_SDK_MacOS_9.1.2.tar.gz": (
        {"kind": "direct", "path": "RED_SDK_MacOS_9.1.2.tar.gz"},
        {"kind": "direct", "path": "RED_SDK_MacOS.tar.gz"},
    ),
    "RED_SDK_Linux_9.1.2.tar.gz": (
        {"kind": "direct", "path": "RED_SDK_Linux_9.1.2.tar.gz"},
        {"kind": "direct", "path": "RED_SDK_Linux.tar.gz"},
    ),
}

LENS_ASSET_NAME = "gyroflow-niyien-lens.cbor.gz"
LENS_METADATA_ASSET_NAME = "gyroflow-niyien-lens.cbor.gz.json"
# Per-component manifests for the decoupled bundle layout. Each bundle is a
# self-describing immutable directory whose name is derived from the sha256 of
# the file contents only.
LENS_MANIFEST_ASSET_NAME = "gyroflow-niyien-lens-manifest.json"
PLUGIN_MANIFEST_ASSET_NAME = "gyroflow-niyien-plugin-manifest.json"
RELEASE_SUMMARY_ASSET_NAME = "gyroflow-niyien-release-summary.json"

# Publish scope tokens. Allow operators to push only the lens bundle, only the
# plugin bundle, or the full app+lens+plugin set. Hash-based directory reuse
# keeps even the "full" path cheap when content didn't change.
SCOPE_APP = "app"
SCOPE_LENS = "lens"
SCOPE_PLUGIN = "plugin"
PUBLISH_SCOPES = {SCOPE_APP, SCOPE_LENS, SCOPE_PLUGIN}
DEFAULT_SCOPES: tuple[str, ...] = (SCOPE_APP, SCOPE_LENS, SCOPE_PLUGIN)

# Files expected to live inside each bundle directory once the publish has
# completed successfully. Used by `bundle_complete()` to skip uploads when a
# tag-named directory already holds every expected file.
EXPECTED_LENS_FILENAMES: tuple[str, ...] = (
    LENS_ASSET_NAME,
    LENS_METADATA_ASSET_NAME,
    LENS_MANIFEST_ASSET_NAME,
)
EXPECTED_PLUGIN_FILENAMES: tuple[str, ...] = tuple(PLUGIN_ASSET_NAMES) + (PLUGIN_MANIFEST_ASSET_NAME,)
DEFAULT_SDK_BASE = "https://api.gyroflow.xyz/sdk"
DEFAULT_GLOBAL_PLUGINS_BASE = "https://github.com/NiYien/gyroflow-plugins/releases/latest/download"
DEFAULT_GITHUB_API = "https://api.github.com"
DEFAULT_123_API = "https://open-api.123pan.com"
DEFAULT_PLATFORM = "open_platform"
DEFAULT_PLUGINS_SOURCE_MODE = "release"
PLUGIN_SOURCE_MODES = {"release", "artifact"}
DEFAULT_APP_SOURCE_MODE = "release"
APP_SOURCE_MODES = {"release", "artifact"}
DEFAULT_DOWNLOAD_RETRIES = 5
PAN123_GET_REQUEST_RETRIES = 3
PAN123_GET_REQUEST_RETRY_DELAY_SECONDS = 2.0
PROGRESS_EVENT_PREFIX = "@@CC_EVENT@@"
PROGRESS_MODE = os.environ.get("NIYIEN_PROGRESS_MODE", "").strip().lower()


def emit_event(payload: dict[str, Any]) -> None:
    if PROGRESS_MODE != "jsonl":
        return
    print(f"{PROGRESS_EVENT_PREFIX}{json.dumps(payload, ensure_ascii=False)}", flush=True)


def emit_progress(
    *,
    phase: str,
    label: str = "",
    message: str = "",
    current: int | None = None,
    total: int | None = None,
    mode: str = "",
) -> None:
    payload: dict[str, Any] = {
        "phase": str(phase).strip(),
        "label": str(label).strip(),
        "message": str(message).strip(),
    }
    if current is not None:
        payload["current"] = int(current)
    if total is not None:
        payload["total"] = int(total)
    if mode:
        payload["mode"] = str(mode).strip()
    emit_event(payload)


def emit_log(message: str) -> None:
    print(f"[publish_pan123_release] {message}", flush=True)


def format_bytes(size: int) -> str:
    units = ("B", "KB", "MB", "GB", "TB")
    value = float(max(int(size), 0))
    unit = units[0]
    for candidate in units:
        unit = candidate
        if value < 1024.0 or candidate == units[-1]:
            break
        value /= 1024.0
    if unit == "B":
        return f"{int(value)} {unit}"
    return f"{value:.2f} {unit}"


def short_id(value: str, keep: int = 8) -> str:
    text = str(value or "").strip()
    if len(text) <= keep:
        return text
    return f"{text[:keep]}..."


def normalize_base_url(value: str, fallback: str, name: str) -> str:
    fallback_base = str(fallback or "").strip().rstrip("/")
    raw_value = str(value or "").strip()
    candidate_values = [raw_value] if raw_value else []
    if raw_value and "://" not in raw_value and not raw_value.startswith("/"):
        candidate_values.append(f"https://{raw_value}")
    candidate_values.append(fallback_base)

    for candidate in candidate_values:
        parsed = urlparse(candidate)
        if parsed.scheme in {"http", "https"} and parsed.netloc:
            normalized = candidate.rstrip("/")
            if raw_value and normalized != raw_value.rstrip("/"):
                emit_log(f"Invalid {name} {raw_value!r}, fallback to {normalized!r}")
            return normalized

    raise RuntimeError(f"Invalid {name}: {raw_value or fallback_base!r}")


def normalize_choice(value: str, *, default: str, name: str, allowed: set[str]) -> str:
    choice = str(value or "").strip().lower() or default
    if choice not in allowed:
        raise RuntimeError(f"Invalid {name}: {choice!r}. Allowed: {', '.join(sorted(allowed))}")
    return choice


def parse_csv_list(value: str) -> list[str]:
    return [item.strip() for item in str(value or "").split(",") if item.strip()]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    # `--app-tag` is required only when the publish scope includes "app".
    # For lens-only or plugin-only publishes, no app installer directory is
    # touched, so an empty value is fine. Validated in main() against scope.
    parser.add_argument("--app-tag", default="")
    parser.add_argument("--workspace", default=".")
    parser.add_argument("--output-dir", default="_deployment/_publish")
    parser.add_argument(
        "--app-source-mode",
        default=(os.environ.get("NIYIEN_APP_SOURCE_MODE", "") or DEFAULT_APP_SOURCE_MODE),
    )
    parser.add_argument("--app-owner", default=os.environ.get("NIYIEN_APP_OWNER", "").strip())
    parser.add_argument("--app-repo", default=os.environ.get("NIYIEN_APP_REPO", "").strip())
    parser.add_argument("--app-run-id", type=int, default=int(os.environ.get("NIYIEN_APP_RUN_ID", "0") or "0"))
    parser.add_argument("--lens-owner", default=os.environ.get("NIYIEN_LENS_DATA_OWNER", "NiYien"))
    parser.add_argument("--lens-repo", default=os.environ.get("NIYIEN_LENS_DATA_REPO", "niyien-lens-data"))
    parser.add_argument("--lens-tag", default=os.environ.get("NIYIEN_LENS_DATA_TAG", "").strip())
    parser.add_argument("--plugins-owner", default=os.environ.get("NIYIEN_PLUGINS_OWNER", "NiYien"))
    parser.add_argument("--plugins-repo", default=os.environ.get("NIYIEN_PLUGINS_REPO", "gyroflow-plugins"))
    parser.add_argument("--plugins-tag", default=os.environ.get("NIYIEN_PLUGINS_TAG", "").strip())
    parser.add_argument(
        "--plugins-source-mode",
        default=(os.environ.get("NIYIEN_PLUGINS_SOURCE_MODE", "") or DEFAULT_PLUGINS_SOURCE_MODE),
    )
    parser.add_argument(
        "--plugins-artifact-name",
        default=os.environ.get("NIYIEN_PLUGINS_ARTIFACT_NAME", "").strip(),
    )
    parser.add_argument(
        "--sdk-base",
        default=(os.environ.get("NIYIEN_SDK_BASE", "") or DEFAULT_SDK_BASE),
    )
    parser.add_argument(
        "--scope",
        default=(os.environ.get("NIYIEN_PUBLISH_SCOPE", "") or ",".join(DEFAULT_SCOPES)),
        help="Comma-separated subset of: app,lens,plugin (default: all three)",
    )
    parser.add_argument("--skip-lens", action="store_true",
                        help="Drop lens from --scope after parsing")
    parser.add_argument("--skip-plugin", action="store_true",
                        help="Drop plugin from --scope after parsing")
    return parser.parse_args()


def parse_publish_scope(raw: str, *, skip_lens: bool, skip_plugin: bool) -> list[str]:
    tokens = [item.strip().lower() for item in str(raw or "").split(",") if item.strip()]
    if not tokens:
        tokens = list(DEFAULT_SCOPES)
    invalid = [tok for tok in tokens if tok not in PUBLISH_SCOPES]
    if invalid:
        raise RuntimeError(
            f"Invalid scope tokens {invalid!r}. Allowed: {sorted(PUBLISH_SCOPES)}"
        )
    seen: list[str] = []
    for tok in tokens:
        if tok not in seen:
            seen.append(tok)
    if skip_lens:
        seen = [tok for tok in seen if tok != SCOPE_LENS]
    if skip_plugin:
        seen = [tok for tok in seen if tok != SCOPE_PLUGIN]
    if not seen:
        raise RuntimeError("Empty publish scope after applying --skip-* flags")
    return seen


@dataclass
class DownloadedFile:
    logical_path: str
    local_path: Path
    source: str
    source_tag: str
    size: int
    sha256: str


@dataclass
class PluginSource:
    mode: str
    source_ref: str
    display_name: str
    owner: str
    repo: str
    release: dict[str, Any] | None = None
    run_id: int = 0
    artifact_names: tuple[str, ...] = ()
    branch: str = ""
    resolved_files: dict[str, Path] | None = None


class GitHubClient:
    def __init__(self, token: str = "") -> None:
        self.base_headers = {
            "Accept": "application/vnd.github+json",
            "User-Agent": "niyien-pan123-publisher",
        }
        self.session = requests.Session()
        self.session.headers.update(self.base_headers)
        if token:
            self.session.headers["Authorization"] = f"Bearer {token}"

    def _get(self, url: str, *, params: dict[str, Any] | None = None, stream: bool = False, timeout: int = 60,
             extra_headers: dict[str, str] | None = None):
        headers = dict(extra_headers) if extra_headers else None
        response = self.session.get(url, params=params, timeout=timeout, stream=stream, headers=headers)
        if response.status_code in {403, 404} and "Authorization" in self.session.headers:
            response.close()
            unauth_headers = dict(self.base_headers)
            if extra_headers:
                unauth_headers.update(extra_headers)
            response = requests.get(
                url,
                params=params,
                timeout=timeout,
                stream=stream,
                headers=unauth_headers,
            )
        return response

    def get_release(self, owner: str, repo: str, tag: str = "") -> dict[str, Any]:
        if tag:
            url = f"{DEFAULT_GITHUB_API}/repos/{owner}/{repo}/releases/tags/{tag}"
        else:
            url = f"{DEFAULT_GITHUB_API}/repos/{owner}/{repo}/releases/latest"
        response = self._get(url, timeout=60)
        response.raise_for_status()
        return response.json()

    def download_asset(self, asset_url: str, destination: Path) -> None:
        # Force identity transfer — proxies in the wild (Clash, V2Ray)
        # sometimes strip Content-Encoding while keeping a compressed body,
        # which then lands as raw zlib bytes that requests can't unwrap.
        # Telling GitHub "don't compress" makes the wire bytes match what
        # we expect to write to disk regardless of what the proxy does.
        self._download_stream_with_retry(
            lambda: self._get(
                asset_url, timeout=300, stream=True,
                extra_headers={"Accept-Encoding": "identity"},
            ),
            destination,
            label=asset_url,
        )

    def get_repository(self, owner: str, repo: str) -> dict[str, Any]:
        url = f"{DEFAULT_GITHUB_API}/repos/{owner}/{repo}"
        response = self._get(url, timeout=60)
        response.raise_for_status()
        return response.json()

    def list_workflow_runs(
        self,
        owner: str,
        repo: str,
        *,
        branch: str = "",
        per_page: int = 20,
    ) -> list[dict[str, Any]]:
        url = f"{DEFAULT_GITHUB_API}/repos/{owner}/{repo}/actions/runs"
        params: dict[str, Any] = {
            "per_page": max(1, min(int(per_page), 100)),
            "exclude_pull_requests": "true",
            "status": "completed",
        }
        if branch:
            params["branch"] = branch
        response = self._get(url, params=params, timeout=60)
        response.raise_for_status()
        payload = response.json()
        runs = payload.get("workflow_runs") if isinstance(payload, dict) else []
        return [item for item in runs or [] if isinstance(item, dict)]

    def list_workflow_run_artifacts(self, owner: str, repo: str, run_id: int) -> list[dict[str, Any]]:
        url = f"{DEFAULT_GITHUB_API}/repos/{owner}/{repo}/actions/runs/{int(run_id)}/artifacts"
        response = self._get(url, params={"per_page": 100}, timeout=60)
        response.raise_for_status()
        payload = response.json()
        artifacts = payload.get("artifacts") if isinstance(payload, dict) else []
        return [item for item in artifacts or [] if isinstance(item, dict)]

    def download_artifact_archive(self, archive_url: str, destination: Path) -> None:
        # See download_asset for why Accept-Encoding: identity is forced.
        # Cache-Control / Pragma defeat any proxy that may be returning
        # a stale cached body for this signed URL.
        self._download_stream_with_retry(
            lambda: self._get(
                archive_url, timeout=300, stream=True,
                extra_headers={
                    "Accept-Encoding": "identity",
                    "Cache-Control": "no-cache, no-store",
                    "Pragma": "no-cache",
                },
            ),
            destination,
            label=archive_url,
        )

    def _download_stream_with_retry(self, opener, destination: Path, *, label: str) -> None:
        last_error = ""

        def _safe_unlink(path: Path) -> None:
            # On Windows the AV / a previous handle from this same process
            # may briefly hold the file. Retry a few times before giving up;
            # don't fail the whole publish over a stale temp file.
            for _ in range(8):
                try:
                    path.unlink(missing_ok=True)
                    return
                except PermissionError:
                    time.sleep(0.25)
            # Last resort: fall through; the open(..., "wb") below will
            # truncate-or-overwrite the file content, which is what we want.

        for attempt in range(1, DEFAULT_DOWNLOAD_RETRIES + 1):
            destination.parent.mkdir(parents=True, exist_ok=True)
            _safe_unlink(destination)
            attempt_label = f" attempt {attempt}/{DEFAULT_DOWNLOAD_RETRIES}" if attempt > 1 else ""
            start_ts = time.time()
            try:
                with opener() as response:
                    response.raise_for_status()
                    total_bytes = int(response.headers.get("Content-Length", 0) or 0)
                    total_mb = total_bytes / (1024 * 1024) if total_bytes else 0
                    pretty_total = f"{total_mb:.1f} MB" if total_bytes else "?"
                    # Diagnostic: surface what the proxy/server is actually
                    # claiming about content encoding + which final URL
                    # we ended up at after redirects. If the final URL
                    # isn't on pipelines.actions.githubusercontent.com (or
                    # objects-origin.githubusercontent.com), a proxy or
                    # CDN may have rewritten the redirect.
                    ce_header = response.headers.get("Content-Encoding", "")
                    ct_header = response.headers.get("Content-Type", "")
                    final_url = str(getattr(response, "url", "") or "")
                    final_url_short = final_url[:120] + "..." if len(final_url) > 120 else final_url
                    emit_log(
                        f"GET {label}{attempt_label} (size={pretty_total}, "
                        f"Content-Type={ct_header or '(none)'}, "
                        f"Content-Encoding={ce_header or '(none)'}, "
                        f"status={response.status_code}, redirected_to={final_url_short})"
                    )
                    emit_progress(
                        phase="download",
                        label=destination.name,
                        message=f"download start (size={pretty_total})",
                        current=0,
                        total=max(total_bytes, 1),
                    )
                    written = 0
                    last_emit = 0
                    with destination.open("wb") as fh:
                        for chunk in response.iter_content(chunk_size=1024 * 1024):
                            if chunk:
                                fh.write(chunk)
                                written += len(chunk)
                                # Throttle: emit every 4 MB or once per chunk
                                # if total unknown, to keep UI snappy without
                                # spamming the JSONL stream.
                                if written - last_emit >= 4 * 1024 * 1024 or total_bytes == 0:
                                    last_emit = written
                                    emit_progress(
                                        phase="download",
                                        label=destination.name,
                                        message=(
                                            f"{written / 1024 / 1024:.1f}/{total_mb:.1f} MB"
                                            if total_bytes
                                            else f"{written / 1024 / 1024:.1f} MB (size unknown)"
                                        ),
                                        current=written,
                                        total=max(total_bytes, written + 1),
                                    )
                if destination.exists() and destination.stat().st_size > 0:
                    file_mb = destination.stat().st_size / 1024 / 1024
                    elapsed_s = max(time.time() - start_ts, 0.001)
                    speed_mbps = file_mb / elapsed_s
                    emit_log(
                        f"Downloaded {destination.name}: {file_mb:.1f} MB in {elapsed_s:.1f}s "
                        f"= {speed_mbps:.2f} MB/s "
                        f"({'⚠ SLOW — check proxy' if speed_mbps < 0.5 else 'OK'})"
                    )
                    return
                last_error = "downloaded file is empty"
            except Exception as err:
                last_error = str(err)
                emit_log(f"Download attempt {attempt} failed: {err}")
            _safe_unlink(destination)
            if attempt < DEFAULT_DOWNLOAD_RETRIES:
                time.sleep(min(2 * attempt, 10))
        raise RuntimeError(f"Failed to download {label}: {last_error}")


class Pan123Client:
    def __init__(self, client_id: str, client_secret: str, releases_root_id: int) -> None:
        self.client_id = client_id.strip()
        self.client_secret = client_secret.strip()
        self.releases_root_id = int(releases_root_id)
        self.session = requests.Session()
        # Disable env-based proxies for 123 网盘 — GitHub uploads/downloads
        # may need a proxy to bypass GFW, but 123 网盘 is fastest direct.
        # Mixing both in the same subprocess env (HTTP_PROXY=...) would
        # otherwise route 123 traffic through the proxy too.
        self.session.trust_env = False
        self.session.headers.update({"User-Agent": "niyien-pan123-publisher"})
        self._token = ""
        self._token_expires_at = 0.0

    def ensure_release_dir(self, name: str) -> int:
        existing = self.find_child(self.releases_root_id, name, expected_type=1)
        if existing:
            emit_log(
                f"123 target dir reused: name={name}, parent={self.releases_root_id}, dir_id={existing['fileId']}"
            )
            return int(existing["fileId"])

        data = self.request(
            "POST",
            "/upload/v1/file/mkdir",
            json_body={"name": name, "parentID": self.releases_root_id},
        )
        emit_log(
            f"123 target dir created: name={name}, parent={self.releases_root_id}, dir_id={data['dirID']}"
        )
        return int(data["dirID"])

    def ensure_release_dir_in(self, parent_id: int, name: str) -> int:
        existing = self.find_child(parent_id, name, expected_type=1)
        if existing:
            emit_log(
                f"123 nested dir reused: name={name}, parent={int(parent_id)}, dir_id={existing['fileId']}"
            )
            return int(existing["fileId"])

        data = self.request(
            "POST",
            "/upload/v1/file/mkdir",
            json_body={"name": name, "parentID": int(parent_id)},
        )
        emit_log(
            f"123 nested dir created: name={name}, parent={int(parent_id)}, dir_id={data['dirID']}"
        )
        return int(data["dirID"])

    def upload_file(self, parent_id: int, local_path: Path, remote_name: str | None = None) -> int:
        remote_name = remote_name or local_path.name
        file_size = local_path.stat().st_size
        file_md5 = md5_file(local_path)
        last_error = ""
        for upload_attempt in range(1, 4):
            emit_log(
                f"123 upload start: file={remote_name}, size={format_bytes(file_size)}, "
                f"parent={int(parent_id)}, attempt={upload_attempt}/3"
            )
            try:
                create_data = self.request(
                    "POST",
                    "/upload/v2/file/create",
                    json_body={
                        "parentFileID": int(parent_id),
                        "filename": remote_name,
                        "etag": file_md5,
                        "size": file_size,
                        "duplicate": 2,
                    },
                )

                if create_data.get("reuse"):
                    emit_log(
                        f"123 upload reused existing file: file={remote_name}, "
                        f"file_id={create_data.get('fileID', 0)}"
                    )
                    return int(create_data.get("fileID", 0))

                preupload_id = str(create_data.get("preuploadID", "")).strip()
                slice_size = int(create_data.get("sliceSize", 0))
                servers = create_data.get("servers") or []
                emit_log(
                    f"123 upload create ok: file={remote_name}, preupload={short_id(preupload_id)}, "
                    f"slice_size={format_bytes(slice_size)}, servers={len(servers)}"
                )
                if not preupload_id or slice_size <= 0:
                    raise RuntimeError(f"Invalid 123 create-file response for {remote_name}")

                if not servers:
                    emit_log(f"123 upload create returned no servers, requesting upload domains: file={remote_name}")
                    domain_data = self.request("GET", "/upload/v2/file/domain")
                    servers = domain_data if isinstance(domain_data, list) else []
                if not servers:
                    raise RuntimeError(f"123 did not return any upload server for {remote_name}")

                upload_bases = [str(item).rstrip("/") for item in servers if str(item).strip()]
                self._upload_slices(upload_bases, local_path, preupload_id, slice_size)

                emit_log(f"123 upload slice phase finished: file={remote_name}, polling upload_complete")
                for finalize_attempt in range(1, 121):
                    try:
                        complete_data = self.request(
                            "POST",
                            "/upload/v2/file/upload_complete",
                            json_body={"preuploadID": preupload_id},
                        )
                    except RuntimeError as err:
                        last_error = str(err)
                        if finalize_attempt in {1, 5, 15, 30, 60, 120}:
                            emit_log(
                                f"123 upload_complete pending: file={remote_name}, "
                                f"attempt={finalize_attempt}/120, detail={last_error}"
                            )
                        # 20103 = 文件正在校验中,请稍后 — explicitly the
                        # retry-friendly case. Service is hashing slices
                        # server-side; back off slightly longer so we
                        # don't hammer the API.
                        sleep_s = 2.0 if "20103" in last_error else 1.0
                        time.sleep(sleep_s)
                        continue
                    if bool(complete_data.get("completed")) and int(complete_data.get("fileID", 0)) > 0:
                        emit_log(
                            f"123 upload finished: file={remote_name}, "
                            f"file_id={complete_data['fileID']}, finalize_attempt={finalize_attempt}"
                        )
                        return int(complete_data["fileID"])
                    if finalize_attempt in {1, 5, 15, 30, 60, 120}:
                        emit_log(
                            f"123 upload_complete not ready yet: file={remote_name}, "
                            f"attempt={finalize_attempt}/120"
                        )
                    time.sleep(1)

                last_error = f"Timed out while finalizing 123 upload for {remote_name}"
            except RuntimeError as err:
                last_error = str(err)
                emit_log(f"123 upload error: file={remote_name}, attempt={upload_attempt}/3, detail={last_error}")
            if upload_attempt < 3:
                emit_log(f"123 upload retry scheduled: file={remote_name}, next_attempt={upload_attempt + 1}/3")
                time.sleep(min(3 * upload_attempt, 10))

        raise RuntimeError(f"123 upload failed for {remote_name}: {last_error}")

    def find_child(self, parent_id: int, name: str, expected_type: int) -> dict[str, Any] | None:
        entries = self.list_directory(parent_id)
        matched = [
            item
            for item in entries
            if str(item.get("filename", "")) == name
            and int(item.get("type", -1)) == expected_type
            and int(item.get("trashed", 0)) == 0
        ]
        if not matched:
            return None
        matched.sort(
            key=lambda item: (
                str(item.get("updateAt", "")),
                int(item.get("fileId", 0)),
            ),
            reverse=True,
        )
        return matched[0]

    def list_directory(self, parent_id: int) -> list[dict[str, Any]]:
        results: list[dict[str, Any]] = []
        last_file_id: int | None = None

        while True:
            params: dict[str, Any] = {
                "parentFileId": int(parent_id),
                "limit": 100,
            }
            if last_file_id is not None and last_file_id != -1:
                params["lastFileId"] = last_file_id

            data = self.request("GET", "/api/v2/file/list", params=params)
            file_list = data.get("fileList") if isinstance(data, dict) else None
            if isinstance(file_list, list):
                results.extend(file_list)

            next_file_id = data.get("lastFileId") if isinstance(data, dict) else -1
            try:
                next_file_id = int(next_file_id)
            except (TypeError, ValueError):
                next_file_id = -1
            if next_file_id == -1:
                break
            last_file_id = next_file_id

        return results

    def request(
        self,
        method: str,
        path: str,
        *,
        params: dict[str, Any] | None = None,
        json_body: dict[str, Any] | None = None,
        auth: bool = True,
    ) -> dict[str, Any] | list[Any]:
        headers = {"Platform": DEFAULT_PLATFORM}
        if auth:
            headers["Authorization"] = f"Bearer {self.get_access_token()}"
        if json_body is not None:
            headers["Content-Type"] = "application/json"

        url = f"{DEFAULT_123_API}{path}"
        method = method.upper()
        max_attempts = PAN123_GET_REQUEST_RETRIES if method == "GET" else 1
        for attempt in range(1, max_attempts + 1):
            try:
                response = self.session.request(
                    method=method,
                    url=url,
                    params=params,
                    json=json_body,
                    headers=headers,
                    timeout=120,
                )
                response.raise_for_status()
                break
            except requests.RequestException as err:
                if attempt < max_attempts and is_retryable_pan123_request_error(err):
                    emit_log(
                        f"123 request retry: {method} {path}, "
                        f"attempt={attempt}/{max_attempts}, detail={err}"
                    )
                    time.sleep(min(PAN123_GET_REQUEST_RETRY_DELAY_SECONDS * attempt, 10.0))
                    continue
                raise RuntimeError(
                    f"123 request failed: {method} {path}, http_error={err}"
                ) from err
        try:
            payload = response.json()
        except ValueError as err:
            preview = response.text[:200].replace("\n", " ")
            raise RuntimeError(
                f"123 response is not valid JSON: {method} {path}, status={response.status_code}, body={preview!r}"
            ) from err
        if int(payload.get("code", -1)) != 0:
            raise RuntimeError(
                f"123 API error {payload.get('code')}: {payload.get('message', 'unknown error')} "
                f"({method} {path})"
            )
        return payload.get("data")

    def get_access_token(self) -> str:
        now = time.time()
        if self._token and self._token_expires_at - 60 > now:
            return self._token

        emit_log("123 requesting access token")
        response = self.session.post(
            f"{DEFAULT_123_API}/api/v1/access_token",
            json={
                "clientID": self.client_id,
                "clientSecret": self.client_secret,
            },
            headers={"Platform": DEFAULT_PLATFORM, "Content-Type": "application/json"},
            timeout=60,
        )
        response.raise_for_status()
        payload = response.json()
        if int(payload.get("code", -1)) != 0:
            raise RuntimeError(
                f"123 token error {payload.get('code')}: {payload.get('message', 'unknown error')}"
            )
        data = payload.get("data") or {}
        access_token = str(data.get("accessToken", "")).strip()
        if not access_token:
            raise RuntimeError("123 token response missing accessToken")

        expired_at = data.get("expiredAt")
        expires_ts = parse_iso_timestamp(expired_at)
        self._token = access_token
        self._token_expires_at = expires_ts or now + 300
        emit_log(
            f"123 access token ready, expires_at={expired_at or 'unknown'}"
        )
        return self._token

    def _upload_slices(self, upload_bases: list[str], local_path: Path, preupload_id: str, slice_size: int) -> None:
        if not upload_bases:
            raise RuntimeError(f"No upload server available for {local_path.name}")
        total_slices = max(1, (local_path.stat().st_size + slice_size - 1) // slice_size)
        emit_log(
            f"123 slice upload start: file={local_path.name}, total_slices={total_slices}, "
            f"slice_size={format_bytes(slice_size)}, servers={len(upload_bases)}, preupload={short_id(preupload_id)}"
        )
        with local_path.open("rb") as fh:
            slice_no = 1
            while True:
                chunk = fh.read(slice_size)
                if not chunk:
                    break

                files = {
                    "slice": (
                        f"{local_path.name}.part{slice_no}",
                        chunk,
                        "application/octet-stream",
                    )
                }
                data = {
                    "preuploadID": preupload_id,
                    "sliceNo": str(slice_no),
                    "sliceMD5": hashlib.md5(chunk).hexdigest(),
                }
                last_error = ""
                for attempt in range(1, 6):
                    upload_base = upload_bases[(slice_no + attempt - 2) % len(upload_bases)]
                    url = f"{upload_base}/upload/v2/file/slice"
                    try:
                        response = self.session.post(
                            url,
                            data=data,
                            files=files,
                            headers={
                                "Authorization": f"Bearer {self.get_access_token()}",
                                "Platform": DEFAULT_PLATFORM,
                            },
                            timeout=900,
                        )
                        response.raise_for_status()
                        payload = response.json()
                        if int(payload.get("code", -1)) != 0:
                            raise RuntimeError(
                                f"123 slice upload failed for {local_path.name}: "
                                f"{payload.get('message', 'unknown error')}"
                            )
                        last_error = ""
                        if slice_no == 1 or slice_no == total_slices or slice_no % 20 == 0:
                            emit_log(
                                f"123 slice upload ok: file={local_path.name}, slice={slice_no}/{total_slices}, "
                                f"server={upload_base}"
                            )
                        break
                    except (requests.Timeout, requests.ConnectionError, requests.HTTPError, RuntimeError) as err:
                        last_error = f"slice {slice_no}, attempt {attempt}, server {upload_base}: {err}"
                        emit_log(
                            f"123 slice upload retry: file={local_path.name}, "
                            f"slice={slice_no}/{total_slices}, attempt={attempt}/5, detail={err}"
                        )
                        if attempt >= 5:
                            raise RuntimeError(
                                f"123 slice upload failed for {local_path.name}: {last_error}"
                            ) from err
                        time.sleep(min(2 * attempt, 10))
                emit_progress(
                    phase="upload",
                    label=local_path.name,
                    message="upload slices",
                    current=slice_no,
                    total=total_slices,
                )
                slice_no += 1


def is_retryable_pan123_request_error(err: requests.RequestException) -> bool:
    if isinstance(err, (requests.Timeout, requests.ConnectionError)):
        return True
    response = getattr(err, "response", None)
    status_code = getattr(response, "status_code", 0)
    return int(status_code or 0) in {429, 500, 502, 503, 504}


def main() -> int:
    args = parse_args()
    args.sdk_base = normalize_base_url(args.sdk_base, DEFAULT_SDK_BASE, "sdk base URL")
    args.app_source_mode = normalize_choice(
        args.app_source_mode,
        default=DEFAULT_APP_SOURCE_MODE,
        name="app source mode",
        allowed=APP_SOURCE_MODES,
    )
    args.plugins_source_mode = normalize_choice(
        args.plugins_source_mode,
        default=DEFAULT_PLUGINS_SOURCE_MODE,
        name="plugin source mode",
        allowed=PLUGIN_SOURCE_MODES,
    )
    scope = parse_publish_scope(
        args.scope, skip_lens=args.skip_lens, skip_plugin=args.skip_plugin
    )
    if SCOPE_APP in scope and not str(args.app_tag).strip():
        raise RuntimeError("--app-tag is required when scope includes 'app'")
    workspace = Path(args.workspace).resolve()
    output_dir = (workspace / args.output_dir).resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    temp_root = output_dir / "_staging"
    temp_root.mkdir(parents=True, exist_ok=True)
    emit_log(
        f"Publish start: app_tag={args.app_tag}, scope={scope}, "
        f"app_source_mode={args.app_source_mode}, "
        f"plugins_source_mode={args.plugins_source_mode}, output_dir={output_dir}"
    )
    emit_log(
        f"Resource refs: lens_tag={args.lens_tag or 'auto-latest'}, "
        f"plugins_tag={args.plugins_tag or 'auto-latest'}, "
        f"plugins_artifact_name={args.plugins_artifact_name or 'auto-latest'}, sdk_base={args.sdk_base}"
    )

    # Diagnostic: surface proxy state. GitHub uploads/downloads honor
    # HTTP_PROXY (trust_env=True default); 123 网盘 client has trust_env=False
    # so it bypasses the proxy regardless. If GitHub downloads are slow,
    # the proxy is the first thing to check.
    http_proxy = os.environ.get("HTTPS_PROXY") or os.environ.get("HTTP_PROXY") or ""
    if http_proxy:
        emit_log(f"Proxy active: GitHub via {http_proxy} | Pan123 direct (bypassed)")
    else:
        emit_log("Proxy: NOT SET — GitHub will go direct (likely slow if behind GFW)")

    github = GitHubClient(os.environ.get("GITHUB_TOKEN", "").strip())
    pan123 = Pan123Client(
        client_id=require_env("PAN123_CLIENT_ID"),
        client_secret=require_env("PAN123_CLIENT_SECRET"),
        releases_root_id=int(require_env("PAN123_RELEASES_ROOT_ID")),
    )

    finalize_event: dict[str, Any] = {
        "phase": "finalize_summary",
        "scope": list(scope),
    }
    summary_payload: dict[str, Any] = {
        "schema": 1,
        "generated_at": utc_now_iso(),
        "scope": list(scope),
        "global_sdk_base": f"{args.sdk_base.rstrip('/')}/",
    }

    # ---------------- App scope ----------------
    app_packages: dict[str, dict[str, Any]] = {}
    app_source_ref = ""
    global_app_urls: dict[str, dict[str, str]] = {}
    if SCOPE_APP in scope:
        emit_log("Resolving app artifacts")
        emit_progress(phase="resolve", message="resolve app artifacts", mode="indeterminate")
        app_assets = resolve_app_asset_files(
            github=github,
            workspace=workspace,
            temp_root=temp_root,
            app_source_mode=args.app_source_mode,
            app_owner=args.app_owner,
            app_repo=args.app_repo,
            app_run_id=args.app_run_id,
        )
        if not app_assets:
            raise RuntimeError("No app artifacts were found after downloading build outputs")
        app_packages = build_app_packages_metadata(app_assets)
        emit_log("App artifacts ready")
        emit_progress(phase="resolve", message="app artifacts ready")
        app_source_ref, global_app_urls = resolve_app_source(
            app_source_mode=args.app_source_mode,
            app_tag=args.app_tag,
            app_owner=args.app_owner,
            app_repo=args.app_repo,
            app_run_id=args.app_run_id,
            app_assets=app_assets,
        )

        emit_log("Uploading app bundle")
        app_dir_id = pan123.ensure_release_dir(args.app_tag)
        total_app_uploads = len(app_assets)
        for index, (asset_name, asset_path) in enumerate(app_assets.items(), start=1):
            emit_progress(
                phase="upload",
                label=asset_name,
                message="upload app bundle to 123",
                current=index,
                total=max(total_app_uploads, 1),
            )
            pan123.upload_file(app_dir_id, asset_path, asset_name)

        finalize_event["app_tag"] = args.app_tag
        finalize_event["packages"] = app_packages
        summary_payload["app_tag"] = args.app_tag
        summary_payload["app_source_mode"] = args.app_source_mode
        summary_payload["app_source_ref"] = app_source_ref
        summary_payload["global_app_urls"] = global_app_urls
        summary_payload["packages"] = app_packages

    with requests.Session() as session:
        session.trust_env = True
        session.headers["User-Agent"] = "niyien-pan123-publisher"

        # ---------------- Lens scope ----------------
        lens_tag = ""
        lens_metadata: dict[str, Any] = {}
        if SCOPE_LENS in scope:
            emit_log("Resolving lens assets")
            emit_progress(phase="resolve", message="resolve lens assets", mode="indeterminate")
            lens_release = github.get_release(args.lens_owner, args.lens_repo, args.lens_tag)
            downloaded_lens = download_lens_assets(
                github=github, temp_root=temp_root, lens_release=lens_release,
            )
            lens_metadata = json.loads(
                next(
                    item.local_path.read_text(encoding="utf-8")
                    for item in downloaded_lens
                    if item.logical_path == LENS_METADATA_ASSET_NAME
                )
            )
            lens_manifest, lens_tag = build_lens_manifest(
                lens_release=lens_release, downloaded_lens=downloaded_lens,
            )
            lens_manifest_path = output_dir / LENS_MANIFEST_ASSET_NAME
            write_json(lens_manifest_path, lens_manifest)

            existing_lens = pan123.find_child(
                pan123.releases_root_id, lens_tag, expected_type=1
            )
            if existing_lens and bundle_complete(
                pan123, int(existing_lens["fileId"]), EXPECTED_LENS_FILENAMES
            ):
                emit_log(f"Lens bundle reused (skip upload): {lens_tag}")
                emit_progress(phase="upload", label=lens_tag, message="lens bundle reused")
            else:
                emit_log(f"Uploading lens bundle: {lens_tag}")
                lens_dir_id = pan123.ensure_release_dir(lens_tag)
                upload_lens_bundle(pan123, lens_dir_id, downloaded_lens, lens_manifest_path)

            # Stage lens assets for any downstream consumers (workflow logs etc.)
            copy_file(
                next(item.local_path for item in downloaded_lens
                     if item.logical_path == LENS_ASSET_NAME),
                output_dir / LENS_ASSET_NAME,
            )
            copy_file(
                next(item.local_path for item in downloaded_lens
                     if item.logical_path == LENS_METADATA_ASSET_NAME),
                output_dir / LENS_METADATA_ASSET_NAME,
            )

            finalize_event["lens_tag"] = lens_tag
            finalize_event["lens_release_tag"] = str(lens_release.get("tag_name", "")).strip()
            finalize_event["lens_version"] = lens_metadata.get("version")
            finalize_event["lens_sha256"] = lens_metadata.get("sha256")
            summary_payload["lens_tag"] = lens_tag
            summary_payload["lens_release_tag"] = finalize_event["lens_release_tag"]
            summary_payload["lens_version"] = lens_metadata.get("version", "")
            summary_payload["lens_sha256"] = lens_metadata.get("sha256", "")

        # ---------------- Plugin scope ----------------
        plugin_tag = ""
        plugin_source: PluginSource | None = None
        if SCOPE_PLUGIN in scope:
            emit_log("Resolving plugin assets")
            emit_progress(phase="resolve", message="resolve plugin source", mode="indeterminate")
            plugin_source = resolve_plugin_source(
                github=github,
                temp_root=temp_root,
                owner=args.plugins_owner,
                repo=args.plugins_repo,
                source_mode=args.plugins_source_mode,
                tag=args.plugins_tag,
                artifact_name=args.plugins_artifact_name,
            )
            downloaded_plugin = download_plugin_assets(
                github=github, temp_root=temp_root, plugin_source=plugin_source,
            )
            plugin_manifest, plugin_tag = build_plugin_manifest(
                plugin_source=plugin_source, downloaded_plugin=downloaded_plugin,
            )
            plugin_manifest_path = output_dir / PLUGIN_MANIFEST_ASSET_NAME
            write_json(plugin_manifest_path, plugin_manifest)

            existing_plugin = pan123.find_child(
                pan123.releases_root_id, plugin_tag, expected_type=1
            )
            if existing_plugin and bundle_complete(
                pan123, int(existing_plugin["fileId"]), EXPECTED_PLUGIN_FILENAMES
            ):
                emit_log(f"Plugin bundle reused (skip upload): {plugin_tag}")
                emit_progress(phase="upload", label=plugin_tag, message="plugin bundle reused")
            else:
                emit_log(f"Uploading plugin bundle: {plugin_tag}")
                plugin_dir_id = pan123.ensure_release_dir(plugin_tag)
                upload_plugin_bundle(pan123, plugin_dir_id, downloaded_plugin, plugin_manifest_path)

            plugin_release_tag = (
                str(plugin_source.release.get("tag_name", "")).strip()
                if plugin_source.release else ""
            )
            finalize_event["plugin_tag"] = plugin_tag
            finalize_event["plugins_release_tag"] = plugin_release_tag
            finalize_event["plugin_source_mode"] = plugin_source.mode
            finalize_event["plugin_source_ref"] = plugin_source.source_ref
            finalize_event["plugin_source_tag"] = plugin_source.display_name
            summary_payload["plugin_tag"] = plugin_tag
            summary_payload["plugins_release_tag"] = plugin_release_tag
            summary_payload["plugin_source_mode"] = plugin_source.mode
            summary_payload["plugin_source_ref"] = plugin_source.source_ref

        # ---------------- SDK (always tag-along) ----------------
        # SDK is shared across releases. Always upload when the publish has
        # any content scope; cheap because find_child skips already-present
        # files. Skip when scope=[app] only.
        if SCOPE_LENS in scope or SCOPE_PLUGIN in scope:
            sdk_files = download_sdk_assets(
                session=session, temp_root=temp_root, sdk_base=args.sdk_base,
            )
            if sdk_files:
                emit_log("Uploading SDK assets to flat releases/sdk/")
                sdk_dir_id = pan123.ensure_release_dir("sdk")
                total_sdk = len(sdk_files)
                for index, item in enumerate(sdk_files, start=1):
                    sdk_filename = Path(item.logical_path).name
                    emit_progress(
                        phase="upload",
                        label=sdk_filename,
                        message=f"sdk {index}/{total_sdk}",
                        current=index,
                        total=max(total_sdk, 1),
                    )
                    existing = pan123.find_child(sdk_dir_id, sdk_filename, expected_type=0)
                    if existing:
                        emit_log(f"SDK reused (skip): {sdk_filename}")
                        continue
                    pan123.upload_file(sdk_dir_id, item.local_path, sdk_filename)

        # Plugin global URL base — same logic as before, but only when the
        # plugin scope was actually published. When a non-plugin scope publish
        # runs, we don't know (and shouldn't overwrite) the global plugin URL.
        global_plugins_base = ""
        if plugin_source is not None:
            global_plugins_base = derive_global_plugins_base(plugin_source)
            summary_payload["global_plugins_base"] = global_plugins_base
            finalize_event["global_plugins_base"] = global_plugins_base

        finalize_event["global_sdk_base"] = f"{args.sdk_base.rstrip('/')}/"

        summary_path = output_dir / RELEASE_SUMMARY_ASSET_NAME
        write_json(summary_path, summary_payload)

        emit_log("Publish finished")
        emit_progress(phase="finalize", message="publish finished")
        emit_event(finalize_event)
        print(json.dumps(summary_payload, indent=2, ensure_ascii=False))

    return 0


def discover_app_assets(workspace: Path) -> dict[str, Path]:
    found: dict[str, Path] = {}
    for asset_name in APP_ASSET_NAMES:
        matches = sorted(workspace.rglob(asset_name))
        if matches:
            found[asset_name] = matches[0]
    return found


def missing_required_app_assets(
    app_assets: dict[str, Path],
    required_assets: Iterable[str] = REQUIRED_APP_ASSET_NAMES,
) -> list[str]:
    return [asset_name for asset_name in required_assets if asset_name not in app_assets]


def ensure_required_app_assets(
    app_assets: dict[str, Path],
    *,
    context: str,
    required_assets: Iterable[str] = REQUIRED_APP_ASSET_NAMES,
) -> None:
    missing = missing_required_app_assets(app_assets, required_assets)
    if missing:
        raise RuntimeError(f"Missing required app assets in {context}: {', '.join(missing)}")


def resolve_app_asset_files(
    *,
    github: GitHubClient,
    workspace: Path,
    temp_root: Path,
    app_source_mode: str,
    app_owner: str,
    app_repo: str,
    app_run_id: int,
) -> dict[str, Path]:
    mode = normalize_choice(
        app_source_mode,
        default=DEFAULT_APP_SOURCE_MODE,
        name="app source mode",
        allowed=APP_SOURCE_MODES,
    )
    if mode == "release":
        app_assets = discover_app_assets(workspace)
        ensure_required_app_assets(app_assets, context="release workspace")
        return app_assets
    if not app_owner or not app_repo or app_run_id <= 0:
        raise RuntimeError("Artifact app source requires app owner, repo, and run id")
    artifacts = github.list_workflow_run_artifacts(app_owner, app_repo, app_run_id)
    valid_artifacts = [item for item in artifacts if not bool(item.get("expired"))]
    if not valid_artifacts:
        raise RuntimeError(f"No app artifacts available for run {app_run_id}")
    app_assets = resolve_app_assets_from_artifacts(
        github=github,
        temp_root=temp_root,
        run_id=app_run_id,
        artifacts=valid_artifacts,
        source_ref=f"actions-run-{app_run_id}",
    )
    ensure_required_app_assets(app_assets, context=f"workflow run {app_run_id}")
    return app_assets


def resolve_app_source(
    *,
    app_source_mode: str,
    app_tag: str,
    app_owner: str,
    app_repo: str,
    app_run_id: int,
    app_assets: dict[str, Path],
) -> tuple[str, dict[str, dict[str, str]]]:
    mode = normalize_choice(
        app_source_mode,
        default=DEFAULT_APP_SOURCE_MODE,
        name="app source mode",
        allowed=APP_SOURCE_MODES,
    )
    if mode == "release":
        return app_tag, {}
    if not app_owner or not app_repo or app_run_id <= 0:
        raise RuntimeError("Artifact app source requires app owner, repo, and run id")
    return (
        f"actions-run-{app_run_id}",
        build_global_artifact_app_urls(app_tag, app_assets.keys()),
    )


def build_global_artifact_app_urls(
    app_tag: str,
    asset_names: Iterable[str],
    *,
    github_owner: str = "NiYien",
    github_repo: str = "gyroflow",
) -> dict[str, dict[str, str]]:
    # Artifact builds expose nightly.link wrappers for global clients (123 disk
    # is unreachable outside CN). Each short artifact name from the workflow
    # corresponds to exactly one raw deliverable; nightly.link serves the V4
    # wrapper at `<short_name>.zip`, and clients unwrap one level on download.
    if not app_tag:
        return {}
    run_id = app_tag[len("run-"):] if app_tag.startswith("run-") else app_tag
    urls: dict[str, dict[str, str]] = {}
    for asset_name in sorted(asset_names):
        platform = APP_ASSET_PLATFORM_BY_NAME.get(asset_name)
        if not platform:
            continue
        artifact_name = APP_FILE_TO_ARTIFACT_NAME.get(asset_name)
        if not artifact_name:
            # Platforms without a defined V4 short-name (e.g. linux/android while
            # nightly upload steps are still pending) keep the legacy 123 route
            # so they don't disappear from manifests entirely.
            url = build_download_route_asset_url(app_tag, asset_name)
        else:
            url = (
                f"https://nightly.link/{github_owner}/{github_repo}"
                f"/actions/runs/{run_id}/{artifact_name}.zip"
            )
        role = APP_ASSET_ROLE_BY_NAME.get(asset_name, "package")
        key = "installer_url" if role == "installer" else "package_url"
        urls.setdefault(platform, {})[key] = url
    return urls


def build_download_route_asset_url(app_tag: str, asset_name: str) -> str:
    if not app_tag or not asset_name:
        return ""
    return f"/api/download/app/{quote(app_tag, safe='')}/{quote(asset_name, safe='')}"


def download_lens_assets(
    *,
    github: GitHubClient,
    temp_root: Path,
    lens_release: dict[str, Any],
) -> list[DownloadedFile]:
    """Download the lens cbor + metadata into temp_root, return DownloadedFile list.

    logical_path is the bare filename so the lens bundle can be uploaded
    flat into `releases/lens-<sha12>/`.
    """
    downloads: list[DownloadedFile] = []
    lens_assets = map_assets(lens_release)
    lens_tag = str(lens_release.get("tag_name", "")).strip()
    for asset_name in (LENS_ASSET_NAME, LENS_METADATA_ASSET_NAME):
        asset = lens_assets.get(asset_name)
        if not asset:
            raise RuntimeError(f"Missing {asset_name} in {lens_tag}")
        destination = temp_root / asset_name
        expected = {
            "kind": "github_release_asset",
            "asset_id": int(asset.get("id", 0) or 0),
            "asset_name": asset_name,
            "updated_at": str(asset.get("updated_at", "")).strip(),
            "source_tag": lens_tag,
        }
        if is_cached_file_match(destination, expected):
            emit_log(f"Reusing local asset: {asset_name}")
            emit_progress(phase="download", label=asset_name, message="cache hit, skip download")
        else:
            emit_progress(phase="download", label=asset_name, message="download lens asset")
            github.download_asset(asset["browser_download_url"], destination)
            write_cached_metadata(destination, expected)
        downloads.append(build_downloaded_file(asset_name, destination, "lens", lens_tag))
    return downloads


def download_plugin_assets(
    *,
    github: GitHubClient,
    temp_root: Path,
    plugin_source: PluginSource,
) -> list[DownloadedFile]:
    """Resolve plugin assets locally, return DownloadedFile list.

    logical_path is the bare filename (no `plugins/` prefix), matching the
    new flat layout `releases/plugin-<sha12>/<asset>`.
    """
    downloads: list[DownloadedFile] = []
    emit_progress(phase="extract", label="plugins", message="resolve plugin assets",
                  mode="indeterminate")
    for asset_name, local_path in resolve_plugin_asset_files(
        github=github,
        temp_root=temp_root,
        plugin_source=plugin_source,
    ).items():
        emit_progress(phase="download", label=asset_name, message="plugin asset ready")
        downloads.append(
            build_downloaded_file(asset_name, local_path, "plugin", plugin_source.source_ref)
        )
    return downloads


def download_sdk_assets(
    *,
    session: requests.Session,
    temp_root: Path,
    sdk_base: str,
) -> list[DownloadedFile]:
    """Resolve SDK assets locally. Layout (`releases/sdk/<filename>`) unchanged."""
    sdk_downloads: list[DownloadedFile] = []
    sdk_base = normalize_base_url(sdk_base, DEFAULT_SDK_BASE, "sdk base URL")
    for filename in SDK_FILENAMES:
        destination = temp_root / "sdk" / filename
        # SDK filenames carry their version (e.g. *_5.0.0.tar.gz), so once
        # we've downloaded a copy that matches the expected base+name, it
        # never needs to be redownloaded for subsequent publish runs. Cache
        # key includes sdk_base so a base URL change does force re-fetch.
        expected_sdk = {
            "kind": "sdk_asset",
            "asset_name": filename,
            "sdk_base": sdk_base,
        }
        if is_cached_file_match(destination, expected_sdk):
            emit_log(f"Reusing local SDK asset: {filename}")
            emit_progress(phase="download", label=filename, message="cache hit, skip download")
            source_tag = sdk_base
        else:
            emit_progress(phase="download", label=filename, message="download sdk asset")
            resolved_url = download_sdk_to_path(
                session=session,
                sdk_base=sdk_base,
                logical_filename=filename,
                destination=destination,
            )
            write_cached_metadata(destination, expected_sdk)
            source_tag = resolved_url
        sdk_downloads.append(
            build_downloaded_file(f"sdk/{filename}", destination, "sdk", source_tag)
        )
    return sdk_downloads


def resolve_plugin_source(
    *,
    github: GitHubClient,
    temp_root: Path,
    owner: str,
    repo: str,
    source_mode: str,
    tag: str,
    artifact_name: str,
) -> PluginSource:
    mode = normalize_choice(
        source_mode,
        default=DEFAULT_PLUGINS_SOURCE_MODE,
        name="plugin source mode",
        allowed=PLUGIN_SOURCE_MODES,
    )
    if mode == "release":
        release = github.get_release(owner, repo, tag)
        release_tag = str(release.get("tag_name", "")).strip() or (tag.strip() if tag else "latest")
        return PluginSource(
            mode="release",
            source_ref=release_tag,
            display_name=release_tag,
            owner=owner,
            repo=repo,
            release=release,
        )

    requested_artifacts = parse_csv_list(artifact_name)
    repository = github.get_repository(owner, repo)
    branch = str(repository.get("default_branch", "")).strip()
    if not branch:
        raise RuntimeError(f"Unable to determine default branch for {owner}/{repo}")

    runs = github.list_workflow_runs(owner, repo, branch=branch, per_page=20)
    if not runs:
        raise RuntimeError(f"No workflow runs found for {owner}/{repo} on branch {branch}")

    errors: list[str] = []
    for run in runs:
        if str(run.get("conclusion", "")).lower() != "success":
            continue
        run_id = int(run.get("id", 0) or 0)
        if run_id <= 0:
            continue
        artifacts = github.list_workflow_run_artifacts(owner, repo, run_id)
        valid_artifacts = [
            item
            for item in artifacts
            if not bool(item.get("expired"))
            and (
                not requested_artifacts
                or str(item.get("name", "")).strip() in set(requested_artifacts)
            )
        ]
        if requested_artifacts:
            matched_names = {str(item.get("name", "")).strip() for item in valid_artifacts}
            if not all(name in matched_names for name in requested_artifacts):
                continue
        elif not valid_artifacts:
            continue

        try:
            resolved_files = resolve_plugin_assets_from_artifacts(
                github=github,
                temp_root=temp_root,
                run_id=run_id,
                artifacts=valid_artifacts,
                source_ref=f"actions-run-{run_id}",
            )
            display_name = ", ".join(requested_artifacts) if requested_artifacts else "latest successful run"
            return PluginSource(
                mode="artifact",
                source_ref=f"actions-run-{run_id}",
                display_name=display_name,
                owner=owner,
                repo=repo,
                run_id=run_id,
                artifact_names=tuple(str(item.get("name", "")).strip() for item in valid_artifacts),
                branch=branch,
                resolved_files=resolved_files,
            )
        except RuntimeError as err:
            errors.append(f"run {run_id}: {err}")

    message = (
        f"Unable to resolve plugin artifacts from {owner}/{repo} on branch {branch}. "
        f"Requested artifact names: {', '.join(requested_artifacts) or 'auto'}."
    )
    if errors:
        message = f"{message} Attempts: {' | '.join(errors[:3])}"
    raise RuntimeError(message)


def resolve_plugin_asset_files(
    *,
    github: GitHubClient,
    temp_root: Path,
    plugin_source: PluginSource,
) -> dict[str, Path]:
    if plugin_source.resolved_files:
        return plugin_source.resolved_files

    if plugin_source.mode == "release":
        if not plugin_source.release:
            raise RuntimeError("Plugin release metadata is missing")
        plugin_assets = map_assets(plugin_source.release)
        result: dict[str, Path] = {}
        for asset_name in PLUGIN_ASSET_NAMES:
            asset = plugin_assets.get(asset_name)
            if not asset:
                raise RuntimeError(f"Missing plugin asset {asset_name} in {plugin_source.source_ref}")
            destination = temp_root / "plugins" / asset_name
            expected = {
                "kind": "github_release_asset",
                "asset_id": int(asset.get("id", 0) or 0),
                "asset_name": asset_name,
                "updated_at": str(asset.get("updated_at", "")).strip(),
                "source_tag": plugin_source.source_ref,
            }
            if is_cached_file_match(destination, expected):
                emit_log(f"Reusing local plugin asset: {asset_name}")
            else:
                github.download_asset(asset["browser_download_url"], destination)
                write_cached_metadata(destination, expected)
            result[asset_name] = destination
        return result

    if plugin_source.mode == "artifact":
        artifacts = github.list_workflow_run_artifacts(
            plugin_source.owner,
            plugin_source.repo,
            plugin_source.run_id,
        )
        if plugin_source.artifact_names:
            allowed = set(plugin_source.artifact_names)
            artifacts = [item for item in artifacts if str(item.get("name", "")).strip() in allowed]
        else:
            artifacts = [item for item in artifacts if not bool(item.get("expired"))]
        if not artifacts:
            raise RuntimeError(f"No plugin artifacts available for run {plugin_source.run_id}")
        return resolve_plugin_assets_from_artifacts(
            github=github,
            temp_root=temp_root,
            run_id=plugin_source.run_id,
            artifacts=artifacts,
            source_ref=plugin_source.source_ref,
        )

    raise RuntimeError(f"Unsupported plugin source mode: {plugin_source.mode}")


def resolve_plugin_assets_from_artifacts(
    *,
    github: GitHubClient,
    temp_root: Path,
    run_id: int,
    artifacts: list[dict[str, Any]],
    source_ref: str,
) -> dict[str, Path]:
    archives_root = temp_root / "plugin_artifacts" / str(run_id)
    extract_root = temp_root / "plugin_extract" / str(run_id)
    plugin_root = temp_root / "plugins"
    artifact_signatures = sorted(
        f"{int(item.get('id', 0) or 0)}:{str(item.get('digest', '')).strip()}:{str(item.get('name', '')).strip()}"
        for item in artifacts
    )
    resolved: dict[str, Path] = {}
    reusable = True
    for asset_name in PLUGIN_ASSET_NAMES:
        destination = plugin_root / asset_name
        expected = {
            "kind": "plugin_artifact_output",
            "source_ref": source_ref,
            "artifact_signatures": artifact_signatures,
            "asset_name": asset_name,
        }
        if is_cached_file_match(destination, expected):
            resolved[asset_name] = destination
        else:
            reusable = False
            break
    if reusable and len(resolved) == len(PLUGIN_ASSET_NAMES):
        emit_log(f"Reusing local plugin artifact outputs: {source_ref}")
        return resolved

    shutil.rmtree(archives_root, ignore_errors=True)
    shutil.rmtree(extract_root, ignore_errors=True)
    archives_root.mkdir(parents=True, exist_ok=True)
    extract_root.mkdir(parents=True, exist_ok=True)

    for index, artifact in enumerate(artifacts, start=1):
        archive_url = str(artifact.get("archive_download_url", "")).strip()
        artifact_name = str(artifact.get("name", "")).strip() or f"artifact-{index}"
        if not archive_url:
            continue
        archive_path = archives_root / f"{index:02d}-{artifact_name}.zip"
        github.download_artifact_archive(archive_url, archive_path)
        artifact_extract_dir = extract_root / f"{index:02d}-{artifact_name}"
        artifact_extract_dir.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(archive_path, "r") as zip_file:
            zip_file.extractall(artifact_extract_dir)

    plugin_root.mkdir(parents=True, exist_ok=True)
    resolved = {}
    missing: list[str] = []
    for asset_name in PLUGIN_ASSET_NAMES:
        matches = sorted(extract_root.rglob(asset_name))
        if not matches:
            missing.append(asset_name)
            continue
        destination = plugin_root / asset_name
        copy_file(matches[0], destination)
        write_cached_metadata(
            destination,
            {
                "kind": "plugin_artifact_output",
                "source_ref": source_ref,
                "artifact_signatures": artifact_signatures,
                "asset_name": asset_name,
            },
        )
        resolved[asset_name] = destination

    if missing:
        raise RuntimeError(
            f"Missing plugin files in artifact source {source_ref}: {', '.join(missing)}"
        )

    return resolved


def resolve_app_assets_from_artifacts(
    *,
    github: GitHubClient,
    temp_root: Path,
    run_id: int,
    artifacts: list[dict[str, Any]],
    source_ref: str,
) -> dict[str, Path]:
    """Resolve app installers from a workflow run's artifacts.

    The nightly upload steps in `.github/workflows/release.yml` use
    `actions/upload-artifact@v4` with short artifact names declared in
    `APP_ARTIFACT_NAMES_BY_FILE` (e.g. `gyroflow-niyien-windows` wrapping the
    raw `gyroflow-niyien-windows64.zip`). The GitHub API serves each artifact
    as a one-level zip wrapper, so we download the wrapper to a temp file and
    extract the named raw deliverable into `app_root`.
    """
    app_root = temp_root / "app"
    wrapper_root = temp_root / "_app_wrappers" / str(run_id)
    artifact_signatures = sorted(
        f"{int(item.get('id', 0) or 0)}:{str(item.get('digest', '')).strip()}:{str(item.get('name', '')).strip()}"
        for item in artifacts
    )

    valid_artifacts = [
        a for a in artifacts
        if str(a.get("archive_download_url", "")).strip()
        and str(a.get("name", "")).strip()
        and str(a.get("name", "")).strip() in APP_ARTIFACT_NAMES_BY_FILE
    ]
    if not valid_artifacts:
        raise RuntimeError(f"No usable app artifacts in run {run_id}")

    # Cache reuse: every artifact already on disk (under its raw filename) with
    # matching signature?
    resolved: dict[str, Path] = {}
    reusable = True
    for artifact in valid_artifacts:
        artifact_name = str(artifact["name"]).strip()
        raw_filename = APP_ARTIFACT_NAMES_BY_FILE[artifact_name]
        destination = app_root / raw_filename
        expected = {
            "kind": "app_artifact_output",
            "source_ref": source_ref,
            "artifact_signatures": artifact_signatures,
            "artifact_name": artifact_name,
            "asset_name": raw_filename,
        }
        if is_cached_file_match(destination, expected):
            resolved[raw_filename] = destination
        else:
            reusable = False
            break
    if reusable and resolved:
        emit_log(f"Reusing local app artifact outputs: {source_ref}")
        return resolved

    # Fresh download — fetch wrapper, extract single raw deliverable.
    app_root.mkdir(parents=True, exist_ok=True)
    shutil.rmtree(wrapper_root, ignore_errors=True)
    wrapper_root.mkdir(parents=True, exist_ok=True)
    resolved = {}
    total = len(valid_artifacts)
    emit_log(f"Resolved {total} app artifacts from run {run_id}, downloading via GitHub")
    for index, artifact in enumerate(valid_artifacts, start=1):
        artifact_name = str(artifact["name"]).strip()
        archive_url = str(artifact["archive_download_url"]).strip()
        raw_filename = APP_ARTIFACT_NAMES_BY_FILE[artifact_name]
        wrapper_path = wrapper_root / f"{artifact_name}.zip"
        destination = app_root / raw_filename
        emit_progress(
            phase="resolve",
            label=artifact_name,
            message=f"download artifact {index}/{total}",
            current=index - 1,
            total=total,
        )
        github.download_artifact_archive(archive_url, wrapper_path)
        try:
            with zipfile.ZipFile(wrapper_path, "r") as wrapper:
                names = wrapper.namelist()
                if raw_filename not in names:
                    raise RuntimeError(
                        f"Artifact {artifact_name} wrapper does not contain {raw_filename}; "
                        f"got {names}"
                    )
                # Extract directly to destination path (avoid intermediate dir).
                with wrapper.open(raw_filename) as src, open(destination, "wb") as dst:
                    shutil.copyfileobj(src, dst)
        finally:
            wrapper_path.unlink(missing_ok=True)
        write_cached_metadata(
            destination,
            {
                "kind": "app_artifact_output",
                "source_ref": source_ref,
                "artifact_signatures": artifact_signatures,
                "artifact_name": artifact_name,
                "asset_name": raw_filename,
            },
        )
        resolved[raw_filename] = destination
        emit_progress(
            phase="resolve",
            label=artifact_name,
            message=f"artifact ready {index}/{total}",
            current=index,
            total=total,
        )

    shutil.rmtree(wrapper_root, ignore_errors=True)
    return resolved


def bundle_complete(
    pan123: Pan123Client, dir_id: int, expected_filenames: Iterable[str]
) -> bool:
    """Return True iff every expected filename is already present in dir_id.

    Bundle directories are hash-named, so contents are immutable. If the
    directory exists and every expected file is there, the publish can skip
    the upload entirely (mirrors the SDK reuse pattern at L1029-1032).

    Pan123Client.list_directory walks pages internally, so this is safe for
    any directory size.
    """
    listing = pan123.list_directory(dir_id)
    present = {
        str(item.get("filename", "")).strip()
        for item in listing
        if int(item.get("trashed", 0)) == 0 and int(item.get("type", -1)) == 0
    }
    expected = {name for name in expected_filenames}
    missing = expected - present
    if missing:
        emit_log(f"Bundle dir_id={dir_id} missing files: {sorted(missing)}")
        return False
    return True


def upload_bundle_flat(
    pan123: Pan123Client,
    dir_id: int,
    downloaded: list[DownloadedFile],
    manifest_path: Path,
    manifest_filename: str,
    *,
    label: str,
) -> None:
    """Upload a bundle's manifest + downloaded files flat into dir_id.

    Used by both upload_lens_bundle and upload_plugin_bundle. Files are
    uploaded by their logical_path basename — the new layout has no nested
    subdirectories inside lens-<x>/ or plugin-<x>/.
    """
    total_uploads = len(downloaded) + 1
    emit_progress(
        phase="upload",
        label=manifest_filename,
        message=f"upload {label} manifest",
        current=1,
        total=total_uploads,
    )
    pan123.upload_file(dir_id, manifest_path, manifest_filename)
    for index, item in enumerate(downloaded, start=2):
        remote_name = Path(item.logical_path).name
        emit_progress(
            phase="upload",
            label=remote_name,
            message=f"upload {label} asset to 123",
            current=index,
            total=total_uploads,
        )
        pan123.upload_file(dir_id, item.local_path, remote_name)


def upload_lens_bundle(
    pan123: Pan123Client,
    lens_dir_id: int,
    downloaded_lens: list[DownloadedFile],
    lens_manifest_path: Path,
) -> None:
    upload_bundle_flat(
        pan123, lens_dir_id, downloaded_lens, lens_manifest_path,
        LENS_MANIFEST_ASSET_NAME, label="lens",
    )


def upload_plugin_bundle(
    pan123: Pan123Client,
    plugin_dir_id: int,
    downloaded_plugin: list[DownloadedFile],
    plugin_manifest_path: Path,
) -> None:
    upload_bundle_flat(
        pan123, plugin_dir_id, downloaded_plugin, plugin_manifest_path,
        PLUGIN_MANIFEST_ASSET_NAME, label="plugin",
    )


def _hash_file_entries(downloaded: list[DownloadedFile]) -> tuple[list[dict[str, Any]], str]:
    """Return (sorted file entries, sha256 of canonical JSON of those entries).

    Hash inputs cover ONLY {path, sha256, size}. release tags / source refs
    are intentionally excluded so that re-running CI on the same binaries
    (artifact mode) produces a stable bundle tag.
    """
    file_entries = [
        {"path": Path(item.logical_path).name, "sha256": item.sha256, "size": item.size}
        for item in sorted(downloaded, key=lambda entry: entry.logical_path)
    ]
    payload = {"schema": 2, "files": file_entries}
    digest = hashlib.sha256(
        json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    return file_entries, digest


def build_lens_manifest(
    *, lens_release: dict[str, Any], downloaded_lens: list[DownloadedFile]
) -> tuple[dict[str, Any], str]:
    file_entries, lens_hash = _hash_file_entries(downloaded_lens)
    lens_tag = f"lens-{lens_hash[:12]}"
    manifest = {
        "schema": 2,
        "kind": "lens",
        "generated_at": utc_now_iso(),
        "lens_tag": lens_tag,
        "lens_hash": lens_hash,
        # Metadata only (does not feed the hash) — useful for ops debugging.
        "lens_release_tag": str(lens_release.get("tag_name", "")).strip(),
        "files": file_entries,
    }
    return manifest, lens_tag


def build_plugin_manifest(
    *, plugin_source: PluginSource, downloaded_plugin: list[DownloadedFile]
) -> tuple[dict[str, Any], str]:
    file_entries, plugin_hash = _hash_file_entries(downloaded_plugin)
    plugin_tag = f"plugin-{plugin_hash[:12]}"
    manifest = {
        "schema": 2,
        "kind": "plugin",
        "generated_at": utc_now_iso(),
        "plugin_tag": plugin_tag,
        "plugin_hash": plugin_hash,
        # Metadata only (does not feed the hash).
        "plugin_source_mode": plugin_source.mode,
        "plugin_source_ref": plugin_source.source_ref,
        "plugins_release_tag": (
            str(plugin_source.release.get("tag_name", "")).strip()
            if plugin_source.release else ""
        ),
        "files": file_entries,
    }
    return manifest, plugin_tag


def build_app_packages_metadata(app_assets: dict[str, Path]) -> dict[str, dict[str, Any]]:
    packages: dict[str, dict[str, Any]] = {}

    windows_setup = app_assets.get("gyroflow-niyien-windows64-setup.exe")
    windows_zip = app_assets.get("gyroflow-niyien-windows64.zip")
    if windows_setup or windows_zip:
        windows: dict[str, Any] = {"kind": "web_installer_zip"}
        if windows_setup:
            add_asset_metadata(
                windows,
                prefix="installer",
                asset_name="gyroflow-niyien-windows64-setup.exe",
                asset_path=windows_setup,
            )
        if windows_zip:
            add_asset_metadata(
                windows,
                prefix="package",
                asset_name="gyroflow-niyien-windows64.zip",
                asset_path=windows_zip,
            )
        packages["windows"] = windows

    macos_dmg = app_assets.get("gyroflow-niyien-mac-universal.dmg")
    if macos_dmg:
        macos: dict[str, Any] = {"kind": "dmg"}
        add_asset_metadata(
            macos,
            prefix="package",
            asset_name="gyroflow-niyien-mac-universal.dmg",
            asset_path=macos_dmg,
        )
        packages["macos"] = macos

    linux_appimage = app_assets.get("gyroflow-niyien-linux64.AppImage")
    if linux_appimage:
        linux: dict[str, Any] = {"kind": "appimage"}
        add_asset_metadata(
            linux,
            prefix="package",
            asset_name="gyroflow-niyien-linux64.AppImage",
            asset_path=linux_appimage,
        )
        packages["linux"] = linux

    android_apk = app_assets.get("gyroflow-niyien.apk")
    if android_apk:
        android: dict[str, Any] = {"kind": "apk"}
        add_asset_metadata(
            android,
            prefix="package",
            asset_name="gyroflow-niyien.apk",
            asset_path=android_apk,
        )
        packages["android"] = android

    return packages


def add_asset_metadata(
    target: dict[str, Any],
    *,
    prefix: str,
    asset_name: str,
    asset_path: Path,
) -> None:
    target[f"{prefix}_filename"] = asset_name
    target[f"{prefix}_sha256"] = sha256_file(asset_path)
    target[f"{prefix}_size"] = asset_path.stat().st_size


def derive_global_plugins_base(plugin_source: PluginSource) -> str:
    """Compose the GitHub-side global URL base for plugin assets.

    `release` mode → releases/latest/download/.
    `artifact` mode → nightly.link, where the client unwraps the V4 short
    artifact zip wrapper (see src/nle_plugins.rs::install).
    """
    if plugin_source.mode == "release" and plugin_source.owner and plugin_source.repo:
        return (
            f"https://github.com/{quote(plugin_source.owner, safe='')}/"
            f"{quote(plugin_source.repo, safe='')}/releases/latest/download/"
        )
    if (
        plugin_source.mode == "artifact"
        and plugin_source.owner
        and plugin_source.repo
        and plugin_source.run_id > 0
    ):
        return (
            f"https://nightly.link/{quote(plugin_source.owner, safe='')}/"
            f"{quote(plugin_source.repo, safe='')}/actions/runs/"
            f"{plugin_source.run_id}/"
        )
    return f"{DEFAULT_GLOBAL_PLUGINS_BASE}/"


def build_downloaded_file(logical_path: str, local_path: Path, source: str, source_tag: str) -> DownloadedFile:
    return DownloadedFile(
        logical_path=logical_path,
        local_path=local_path,
        source=source,
        source_tag=source_tag,
        size=local_path.stat().st_size,
        sha256=sha256_file(local_path),
    )


def map_assets(release: dict[str, Any]) -> dict[str, dict[str, Any]]:
    assets = release.get("assets") if isinstance(release, dict) else None
    if not isinstance(assets, list):
        return {}
    return {str(asset.get("name", "")): asset for asset in assets if asset.get("name")}


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, ensure_ascii=False), encoding="utf-8")


def metadata_sidecar_path(path: Path) -> Path:
    return path.with_name(f"{path.name}.meta.json")


def load_cached_metadata(path: Path) -> dict[str, Any]:
    sidecar = metadata_sidecar_path(path)
    if not path.exists() or not sidecar.exists():
        return {}
    try:
        payload = json.loads(sidecar.read_text(encoding="utf-8"))
    except Exception:
        return {}
    return payload if isinstance(payload, dict) else {}


def is_cached_file_match(path: Path, expected: dict[str, Any]) -> bool:
    metadata = load_cached_metadata(path)
    if not metadata:
        return False
    for key, value in expected.items():
        if metadata.get(key) != value:
            return False
    if int(metadata.get("file_size", -1)) != path.stat().st_size:
        return False
    checksum = str(metadata.get("file_sha256", "")).strip()
    if not checksum:
        return False
    return sha256_file(path) == checksum


def write_cached_metadata(path: Path, expected: dict[str, Any]) -> None:
    payload = dict(expected)
    payload["file_size"] = path.stat().st_size
    payload["file_sha256"] = sha256_file(path)
    write_json(metadata_sidecar_path(path), payload)


def fetch_remote_download_signature(session: requests.Session, url: str) -> dict[str, Any]:
    with session.get(url, timeout=60, stream=True) as response:
        response.raise_for_status()
        return {
            "url": url,
            "content_length": int(response.headers.get("Content-Length", "0") or "0"),
            "etag": str(response.headers.get("ETag", "")).strip(),
            "last_modified": str(response.headers.get("Last-Modified", "")).strip(),
        }


def build_sdk_download_candidates(logical_filename: str, sdk_base: str) -> list[dict[str, str]]:
    specs = SDK_DOWNLOAD_SOURCES.get(logical_filename) or (
        {"kind": "direct", "path": logical_filename},
    )
    candidates: list[dict[str, str]] = []
    for spec in specs:
        kind = str(spec.get("kind", "direct")).strip().lower()
        if kind == "direct":
            path = str(spec.get("path", logical_filename)).strip() or logical_filename
            url = path if "://" in path else f"{sdk_base.rstrip('/')}/{path.lstrip('/')}"
            candidates.append({"kind": "direct", "url": url})
        elif kind == "repack_tar_xz":
            url = str(spec.get("url", "")).strip()
            if url:
                candidates.append({"kind": "repack_tar_xz", "url": url})
    return candidates


def download_sdk_to_path(
    *,
    session: requests.Session,
    sdk_base: str,
    logical_filename: str,
    destination: Path,
) -> str:
    attempts: list[str] = []
    for candidate in build_sdk_download_candidates(logical_filename, sdk_base):
        url = candidate["url"]
        try:
            remote_signature = fetch_remote_download_signature(session, url)
            expected = {
                "kind": "sdk_download",
                "logical_filename": logical_filename,
                "source_url": url,
                "remote_signature": remote_signature,
            }
            if is_cached_file_match(destination, expected):
                emit_log(f"Reusing local SDK: {logical_filename}")
                return url
            if candidate["kind"] == "repack_tar_xz":
                download_and_repack_tar_xz(session, url, destination)
            else:
                download_to_path(session, url, destination)
            write_cached_metadata(destination, expected)
            return url
        except requests.HTTPError as err:
            status = getattr(err.response, "status_code", None)
            attempts.append(f"{url} -> HTTP {status or 'error'}")
        except Exception as err:
            attempts.append(f"{url} -> {err}")
        finally:
            if destination.exists() and destination.stat().st_size == 0:
                destination.unlink(missing_ok=True)

    raise RuntimeError(
        f"Unable to download SDK asset {logical_filename}. "
        f"Attempts: {' | '.join(attempts) or 'none'}"
    )


def download_and_repack_tar_xz(session: requests.Session, url: str, destination: Path) -> None:
    temp_archive = destination.parent / f"{destination.name}.source.tar.xz"
    extract_root = destination.parent / f"{destination.name}.extract"
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.rmtree(extract_root, ignore_errors=True)
    temp_archive.unlink(missing_ok=True)
    destination.unlink(missing_ok=True)
    try:
        download_to_path(session, url, temp_archive)
        with tarfile.open(temp_archive, "r:xz") as archive:
            archive.extractall(extract_root)
        with tarfile.open(destination, "w:gz") as archive:
            for path in sorted(extract_root.rglob("*")):
                archive.add(path, arcname=path.relative_to(extract_root))
    finally:
        temp_archive.unlink(missing_ok=True)
        shutil.rmtree(extract_root, ignore_errors=True)


def download_to_path(session: requests.Session, url: str, destination: Path) -> None:
    parsed = urlparse(str(url or "").strip())
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise RuntimeError(f"Invalid download URL: {url!r}")
    last_error = ""
    for attempt in range(1, DEFAULT_DOWNLOAD_RETRIES + 1):
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.unlink(missing_ok=True)
        try:
            with session.get(url, timeout=300, stream=True) as response:
                response.raise_for_status()
                with destination.open("wb") as fh:
                    for chunk in response.iter_content(chunk_size=1024 * 1024):
                        if chunk:
                            fh.write(chunk)
            if destination.exists() and destination.stat().st_size > 0:
                return
            last_error = "downloaded file is empty"
        except Exception as err:
            last_error = str(err)
        destination.unlink(missing_ok=True)
        if attempt < DEFAULT_DOWNLOAD_RETRIES:
            time.sleep(min(2 * attempt, 10))
    raise RuntimeError(f"Failed to download {url}: {last_error}")


def copy_file(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def md5_file(path: Path) -> str:
    digest = hashlib.md5()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_iso_timestamp(value: Any) -> float:
    text = str(value or "").strip()
    if not text:
        return 0.0
    try:
        return datetime.fromisoformat(text.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return 0.0


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def require_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"Missing required environment variable: {name}")
    return value


if __name__ == "__main__":
    raise SystemExit(main())
