#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import time
import zipfile
from urllib.parse import quote, urlparse
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import requests


APP_ASSET_NAMES = (
    "gyroflow-niyien-windows64.zip",
    "gyroflow-niyien-mac-universal.dmg",
    "gyroflow-niyien-linux64.AppImage",
    "gyroflow-niyien.apk",
)
APP_ASSET_PLATFORM_BY_NAME = {
    "gyroflow-niyien-windows64.zip": "windows",
    "gyroflow-niyien-mac-universal.dmg": "macos",
    "gyroflow-niyien-linux64.AppImage": "linux",
    "gyroflow-niyien.apk": "android",
}

PLUGIN_ASSET_NAMES = (
    "Gyroflow-OpenFX-windows.zip",
    "Gyroflow-Adobe-windows.aex",
    "Gyroflow-OpenFX-macos.zip",
    "Gyroflow-Adobe-macos.zip",
)

SDK_FILENAMES = (
    "Blackmagic_RAW_SDK_Windows_5.0.0.tar.gz",
    "Blackmagic_RAW_SDK_MacOS_5.0.0.tar.gz",
    "Blackmagic_RAW_SDK_Linux_5.0.0.tar.gz",
    "RED_SDK_Windows_9.1.2.tar.gz",
    "RED_SDK_MacOS_9.1.2.tar.gz",
    "RED_SDK_Linux_9.1.2.tar.gz",
    "ffmpeg_gpl_Windows.tar.gz",
    "ffmpeg_gpl_MacOS.tar.gz",
    "ffmpeg_gpl_Linux.tar.gz",
)

LENS_ASSET_NAME = "gyroflow-niyien-lens.cbor.gz"
LENS_METADATA_ASSET_NAME = "gyroflow-niyien-lens.cbor.gz.json"
CONTENT_MANIFEST_ASSET_NAME = "gyroflow-niyien-content-manifest.json"
RELEASE_SUMMARY_ASSET_NAME = "gyroflow-niyien-release-summary.json"
DEFAULT_SDK_BASE = "https://api.gyroflow.xyz/sdk"
DEFAULT_GLOBAL_PLUGINS_BASE = "https://github.com/gyroflow/gyroflow-plugins/releases/latest/download"
DEFAULT_GITHUB_API = "https://api.github.com"
DEFAULT_123_API = "https://open-api.123pan.com"
DEFAULT_PLATFORM = "open_platform"
DEFAULT_PLUGINS_SOURCE_MODE = "release"
PLUGIN_SOURCE_MODES = {"release", "artifact"}
DEFAULT_APP_SOURCE_MODE = "release"
APP_SOURCE_MODES = {"release", "artifact"}
DEFAULT_NIGHTLY_LINK_BASE = "https://nightly.link"


def normalize_base_url(value: str, fallback: str, name: str) -> str:
    base = str(value or "").strip() or fallback
    parsed = urlparse(base)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise RuntimeError(f"Invalid {name}: {base!r}")
    return base.rstrip("/")


def normalize_choice(value: str, *, default: str, name: str, allowed: set[str]) -> str:
    choice = str(value or "").strip().lower() or default
    if choice not in allowed:
        raise RuntimeError(f"Invalid {name}: {choice!r}. Allowed: {', '.join(sorted(allowed))}")
    return choice


def parse_csv_list(value: str) -> list[str]:
    return [item.strip() for item in str(value or "").split(",") if item.strip()]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--app-tag", required=True)
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
    parser.add_argument("--plugins-owner", default=os.environ.get("NIYIEN_PLUGINS_OWNER", "gyroflow"))
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
    return parser.parse_args()


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

    def _get(self, url: str, *, params: dict[str, Any] | None = None, stream: bool = False, timeout: int = 60):
        response = self.session.get(url, params=params, timeout=timeout, stream=stream)
        if response.status_code in {403, 404} and "Authorization" in self.session.headers:
            response.close()
            response = requests.get(
                url,
                params=params,
                timeout=timeout,
                stream=stream,
                headers=self.base_headers,
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
        with self._get(asset_url, timeout=300, stream=True) as response:
            response.raise_for_status()
            destination.parent.mkdir(parents=True, exist_ok=True)
            with destination.open("wb") as fh:
                for chunk in response.iter_content(chunk_size=1024 * 1024):
                    if chunk:
                        fh.write(chunk)

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
        with self._get(archive_url, timeout=300, stream=True) as response:
            response.raise_for_status()
            destination.parent.mkdir(parents=True, exist_ok=True)
            with destination.open("wb") as fh:
                for chunk in response.iter_content(chunk_size=1024 * 1024):
                    if chunk:
                        fh.write(chunk)


class Pan123Client:
    def __init__(self, client_id: str, client_secret: str, releases_root_id: int) -> None:
        self.client_id = client_id.strip()
        self.client_secret = client_secret.strip()
        self.releases_root_id = int(releases_root_id)
        self.session = requests.Session()
        self.session.headers.update({"User-Agent": "niyien-pan123-publisher"})
        self._token = ""
        self._token_expires_at = 0.0

    def ensure_release_dir(self, name: str) -> int:
        existing = self.find_child(self.releases_root_id, name, expected_type=1)
        if existing:
            return int(existing["fileId"])

        data = self.request(
            "POST",
            "/upload/v1/file/mkdir",
            json_body={"name": name, "parentID": self.releases_root_id},
        )
        return int(data["dirID"])

    def ensure_release_dir_in(self, parent_id: int, name: str) -> int:
        existing = self.find_child(parent_id, name, expected_type=1)
        if existing:
            return int(existing["fileId"])

        data = self.request(
            "POST",
            "/upload/v1/file/mkdir",
            json_body={"name": name, "parentID": int(parent_id)},
        )
        return int(data["dirID"])

    def upload_file(self, parent_id: int, local_path: Path, remote_name: str | None = None) -> int:
        remote_name = remote_name or local_path.name
        file_size = local_path.stat().st_size
        file_md5 = md5_file(local_path)

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
            return int(create_data.get("fileID", 0))

        preupload_id = str(create_data.get("preuploadID", "")).strip()
        slice_size = int(create_data.get("sliceSize", 0))
        servers = create_data.get("servers") or []
        if not preupload_id or slice_size <= 0:
            raise RuntimeError(f"Invalid 123 create-file response for {remote_name}")

        if not servers:
            domain_data = self.request("GET", "/upload/v2/file/domain")
            servers = domain_data if isinstance(domain_data, list) else []
        if not servers:
            raise RuntimeError(f"123 did not return any upload server for {remote_name}")

        upload_base = str(servers[0]).rstrip("/")
        self._upload_slices(upload_base, local_path, preupload_id, slice_size)

        for _ in range(120):
            complete_data = self.request(
                "POST",
                "/upload/v2/file/upload_complete",
                json_body={"preuploadID": preupload_id},
            )
            if bool(complete_data.get("completed")) and int(complete_data.get("fileID", 0)) > 0:
                return int(complete_data["fileID"])
            time.sleep(1)

        raise RuntimeError(f"Timed out while finalizing 123 upload for {remote_name}")

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

        response = self.session.request(
            method=method,
            url=f"{DEFAULT_123_API}{path}",
            params=params,
            json=json_body,
            headers=headers,
            timeout=120,
        )
        response.raise_for_status()
        payload = response.json()
        if int(payload.get("code", -1)) != 0:
            raise RuntimeError(
                f"123 API error {payload.get('code')}: {payload.get('message', 'unknown error')}"
            )
        return payload.get("data")

    def get_access_token(self) -> str:
        now = time.time()
        if self._token and self._token_expires_at - 60 > now:
            return self._token

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
        return self._token

    def _upload_slices(self, upload_base: str, local_path: Path, preupload_id: str, slice_size: int) -> None:
        url = f"{upload_base}/upload/v2/file/slice"
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
                response = self.session.post(
                    url,
                    data=data,
                    files=files,
                    headers={
                        "Authorization": f"Bearer {self.get_access_token()}",
                        "Platform": DEFAULT_PLATFORM,
                    },
                    timeout=300,
                )
                response.raise_for_status()
                payload = response.json()
                if int(payload.get("code", -1)) != 0:
                    raise RuntimeError(
                        f"123 slice upload failed for {local_path.name}: {payload.get('message', 'unknown error')}"
                    )
                slice_no += 1


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
    workspace = Path(args.workspace).resolve()
    output_dir = (workspace / args.output_dir).resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    github = GitHubClient(os.environ.get("GITHUB_TOKEN", "").strip())
    pan123 = Pan123Client(
        client_id=require_env("PAN123_CLIENT_ID"),
        client_secret=require_env("PAN123_CLIENT_SECRET"),
        releases_root_id=int(require_env("PAN123_RELEASES_ROOT_ID")),
    )

    app_assets = discover_app_assets(workspace)
    if not app_assets:
        raise RuntimeError("No app artifacts were found after downloading build outputs")
    app_source_ref, global_app_urls = resolve_app_source(
        app_source_mode=args.app_source_mode,
        app_tag=args.app_tag,
        app_owner=args.app_owner,
        app_repo=args.app_repo,
        app_run_id=args.app_run_id,
        app_assets=app_assets,
    )

    with requests.Session() as session:
        session.headers["User-Agent"] = "niyien-pan123-publisher"
        temp_root = output_dir / "_staging"
        if temp_root.exists():
            shutil.rmtree(temp_root)
        temp_root.mkdir(parents=True, exist_ok=True)

        lens_release = github.get_release(args.lens_owner, args.lens_repo, args.lens_tag)
        plugin_source = resolve_plugin_source(
            github=github,
            temp_root=temp_root,
            owner=args.plugins_owner,
            repo=args.plugins_repo,
            source_mode=args.plugins_source_mode,
            tag=args.plugins_tag,
            artifact_name=args.plugins_artifact_name,
        )

        downloaded_content = download_content_assets(
            github=github,
            session=session,
            temp_root=temp_root,
            lens_release=lens_release,
            plugin_source=plugin_source,
            sdk_base=args.sdk_base,
        )

        lens_metadata = json.loads(
            next(
                item.local_path.read_text(encoding="utf-8")
                for item in downloaded_content
                if item.logical_path.endswith(LENS_METADATA_ASSET_NAME)
            )
        )

        content_manifest, content_tag = build_content_manifest(
            app_tag=args.app_tag,
            app_source_mode=args.app_source_mode,
            app_source_ref=app_source_ref,
            lens_release=lens_release,
            plugin_source=plugin_source,
            downloaded_files=downloaded_content,
        )
        content_manifest_path = output_dir / CONTENT_MANIFEST_ASSET_NAME
        write_json(content_manifest_path, content_manifest)

        app_dir_id = pan123.ensure_release_dir(args.app_tag)
        for asset_name, asset_path in app_assets.items():
            pan123.upload_file(app_dir_id, asset_path, asset_name)

        content_dir_id = pan123.ensure_release_dir(content_tag)
        upload_content_bundle(pan123, content_dir_id, downloaded_content, content_manifest_path)

        summary = build_release_summary(
            app_tag=args.app_tag,
            app_source_mode=args.app_source_mode,
            app_source_ref=app_source_ref,
            global_app_urls=global_app_urls,
            content_tag=content_tag,
            lens_release=lens_release,
            plugin_source=plugin_source,
            lens_metadata=lens_metadata,
            sdk_base=args.sdk_base,
        )
        summary_path = output_dir / RELEASE_SUMMARY_ASSET_NAME
        write_json(summary_path, summary)

        copy_file(
            next(item.local_path for item in downloaded_content if item.logical_path == LENS_ASSET_NAME),
            output_dir / LENS_ASSET_NAME,
        )
        copy_file(
            next(
                item.local_path
                for item in downloaded_content
                if item.logical_path == LENS_METADATA_ASSET_NAME
            ),
            output_dir / LENS_METADATA_ASSET_NAME,
        )

        shutil.rmtree(temp_root, ignore_errors=True)

        print(json.dumps(summary, indent=2, ensure_ascii=False))

    return 0


def discover_app_assets(workspace: Path) -> dict[str, Path]:
    found: dict[str, Path] = {}
    for asset_name in APP_ASSET_NAMES:
        matches = sorted(workspace.rglob(asset_name))
        if matches:
            found[asset_name] = matches[0]
    return found


def resolve_app_source(
    *,
    app_source_mode: str,
    app_tag: str,
    app_owner: str,
    app_repo: str,
    app_run_id: int,
    app_assets: dict[str, Path],
) -> tuple[str, dict[str, str]]:
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
        build_global_artifact_app_urls(app_owner, app_repo, app_run_id, app_assets),
    )
    
    
def build_global_artifact_app_urls(
    owner: str,
    repo: str,
    run_id: int,
    app_assets: dict[str, Path],
) -> dict[str, str]:
    urls: dict[str, str] = {}
    for asset_name in sorted(app_assets.keys()):
        platform = APP_ASSET_PLATFORM_BY_NAME.get(asset_name)
        if not platform:
            continue
        encoded_name = quote(asset_name, safe="")
        urls[platform] = (
            f"{DEFAULT_NIGHTLY_LINK_BASE}/{quote(owner, safe='')}/{quote(repo, safe='')}"
            f"/actions/runs/{int(run_id)}/{encoded_name}.zip"
        )
    return urls


def download_content_assets(
    *,
    github: GitHubClient,
    session: requests.Session,
    temp_root: Path,
    lens_release: dict[str, Any],
    plugin_source: PluginSource,
    sdk_base: str,
) -> list[DownloadedFile]:
    downloads: list[DownloadedFile] = []

    lens_assets = map_assets(lens_release)
    lens_tag = str(lens_release.get("tag_name", "")).strip()
    for asset_name in (LENS_ASSET_NAME, LENS_METADATA_ASSET_NAME):
        asset = lens_assets.get(asset_name)
        if not asset:
            raise RuntimeError(f"Missing {asset_name} in {lens_tag}")
        destination = temp_root / asset_name
        github.download_asset(asset["browser_download_url"], destination)
        downloads.append(build_downloaded_file(asset_name, destination, "lens", lens_tag))

    for asset_name, local_path in resolve_plugin_asset_files(
        github=github,
        temp_root=temp_root,
        plugin_source=plugin_source,
    ).items():
        downloads.append(
            build_downloaded_file(
                f"plugins/{asset_name}",
                local_path,
                "plugin",
                plugin_source.source_ref,
            )
        )

    sdk_base = normalize_base_url(sdk_base, DEFAULT_SDK_BASE, "sdk base URL")
    for filename in SDK_FILENAMES:
        destination = temp_root / "sdk" / filename
        download_url = f"{sdk_base}/{filename}"
        download_to_path(session, download_url, destination)
        downloads.append(build_downloaded_file(f"sdk/{filename}", destination, "sdk", sdk_base))

    return downloads


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
            github.download_asset(asset["browser_download_url"], destination)
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

    plugin_root = temp_root / "plugins"
    plugin_root.mkdir(parents=True, exist_ok=True)
    resolved: dict[str, Path] = {}
    missing: list[str] = []
    for asset_name in PLUGIN_ASSET_NAMES:
        matches = sorted(extract_root.rglob(asset_name))
        if not matches:
            missing.append(asset_name)
            continue
        destination = plugin_root / asset_name
        copy_file(matches[0], destination)
        resolved[asset_name] = destination

    if missing:
        raise RuntimeError(
            f"Missing plugin files in artifact source {source_ref}: {', '.join(missing)}"
        )

    return resolved


def upload_content_bundle(
    pan123: Pan123Client,
    content_dir_id: int,
    downloaded_content: list[DownloadedFile],
    content_manifest_path: Path,
) -> None:
    pan123.upload_file(content_dir_id, content_manifest_path, CONTENT_MANIFEST_ASSET_NAME)

    subdir_cache: dict[str, int] = {}
    for item in downloaded_content:
        relative_path = Path(item.logical_path)
        parent_id = content_dir_id
        if len(relative_path.parts) > 1:
            for folder in relative_path.parts[:-1]:
                cache_key = f"{parent_id}:{folder}"
                if cache_key not in subdir_cache:
                    subdir_cache[cache_key] = pan123.ensure_release_dir_in(parent_id, folder)
                parent_id = subdir_cache[cache_key]
        pan123.upload_file(parent_id, item.local_path, relative_path.name)


def build_content_manifest(
    *,
    app_tag: str,
    app_source_mode: str,
    app_source_ref: str,
    lens_release: dict[str, Any],
    plugin_source: PluginSource,
    downloaded_files: list[DownloadedFile],
) -> tuple[dict[str, Any], str]:
    file_entries = [
        {
            "path": item.logical_path,
            "sha256": item.sha256,
            "size": item.size,
            "source": item.source,
            "source_tag": item.source_tag,
        }
        for item in sorted(downloaded_files, key=lambda entry: entry.logical_path)
    ]
    hash_payload = {
        "app_tag": app_tag,
        "app_source_mode": app_source_mode,
        "app_source_ref": app_source_ref,
        "lens_release_tag": lens_release.get("tag_name", ""),
        "plugin_source_mode": plugin_source.mode,
        "plugin_source_ref": plugin_source.source_ref,
        "files": file_entries,
    }
    manifest_hash = hashlib.sha256(
        json.dumps(hash_payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    content_tag = f"content-{manifest_hash[:12]}"
    manifest = {
        "schema": 1,
        "generated_at": utc_now_iso(),
        "app_tag": app_tag,
        "content_tag": content_tag,
        "content_hash": manifest_hash,
        "app_source_mode": app_source_mode,
        "app_source_ref": app_source_ref,
        "lens_release_tag": lens_release.get("tag_name", ""),
        "plugin_source_mode": plugin_source.mode,
        "plugin_source_ref": plugin_source.source_ref,
        "plugins_release_tag": plugin_source.release.get("tag_name", "") if plugin_source.release else "",
        "files": file_entries,
    }
    return manifest, content_tag


def build_release_summary(
    *,
    app_tag: str,
    app_source_mode: str,
    app_source_ref: str,
    global_app_urls: dict[str, str],
    content_tag: str,
    lens_release: dict[str, Any],
    plugin_source: PluginSource,
    lens_metadata: dict[str, Any],
    sdk_base: str,
) -> dict[str, Any]:
    return {
        "schema": 1,
        "generated_at": utc_now_iso(),
        "app_tag": app_tag,
        "app_source_mode": app_source_mode,
        "app_source_ref": app_source_ref,
        "global_app_urls": global_app_urls,
        "content_tag": content_tag,
        "lens_version": lens_metadata.get("version", ""),
        "lens_sha256": lens_metadata.get("sha256", ""),
        "lens_source_tag": lens_release.get("tag_name", ""),
        "plugins_source_mode": plugin_source.mode,
        "plugins_source_ref": plugin_source.source_ref,
        "plugins_source_tag": plugin_source.display_name,
        "global_sdk_base": f"{sdk_base.rstrip('/')}/",
        "global_plugins_base": f"{DEFAULT_GLOBAL_PLUGINS_BASE}/",
    }


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


def download_to_path(session: requests.Session, url: str, destination: Path) -> None:
    with session.get(url, timeout=300, stream=True) as response:
        response.raise_for_status()
        destination.parent.mkdir(parents=True, exist_ok=True)
        with destination.open("wb") as fh:
            for chunk in response.iter_content(chunk_size=1024 * 1024):
                if chunk:
                    fh.write(chunk)


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
