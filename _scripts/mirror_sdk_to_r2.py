#!/usr/bin/env python3
from __future__ import annotations

import argparse
import base64
import dataclasses
import hashlib
import hmac
import html.parser
import io
import json
import mimetypes
import os
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import quote, unquote, urljoin, urlparse
from urllib.request import Request, urlopen

import requests


DEFAULT_UPSTREAM_BASE = "https://api.gyroflow.xyz/sdk/"
DEFAULT_PUBLIC_BASE = "https://www.niyien.com/api/sdk/"
DEFAULT_OBJECT_PREFIX = "sdk/"
DEFAULT_R2_REGION = "auto"
DEFAULT_LOCAL_CONFIG = Path.home() / ".config" / "niyien" / "sdk-r2-mirror.json"


@dataclass(frozen=True)
class SdkFile:
    relative_path: str
    source_url: str


@dataclass(frozen=True)
class R2Config:
    r2_account_id: str
    r2_access_key_id: str
    r2_secret_access_key: str
    r2_bucket: str
    r2_region: str = DEFAULT_R2_REGION


@dataclass(frozen=True)
class VerificationResult:
    total: int
    missing_r2: list[str]
    missing_public: list[str]


class IndexParser(html.parser.HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.hrefs: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag.lower() != "a":
            return
        for name, value in attrs:
            if name.lower() == "href" and value:
                self.hrefs.append(value)


class R2S3Client:
    def __init__(
        self,
        *,
        account_id: str,
        access_key_id: str,
        secret_access_key: str,
        bucket: str,
        region: str = DEFAULT_R2_REGION,
    ) -> None:
        self.account_id = account_id
        self.access_key_id = access_key_id
        self.secret_access_key = secret_access_key
        self.bucket = bucket
        self.region = region or DEFAULT_R2_REGION
        self.endpoint = f"https://{account_id}.r2.cloudflarestorage.com"

    def head_object(self, key: str) -> bool:
        request = self._signed_request("HEAD", key, body=b"")
        try:
            with urlopen(request, timeout=60) as response:
                return 200 <= int(response.status) < 300
        except Exception:
            return False

    def put_object(self, *, key: str, body: io.BytesIO, content_type: str = "") -> None:
        payload = body.getvalue()
        headers = {}
        if content_type:
            headers["Content-Type"] = content_type
        request = self._signed_request("PUT", key, body=payload, headers=headers)
        with urlopen(request, timeout=300) as response:
            if not (200 <= int(response.status) < 300):
                raise RuntimeError(f"R2 PUT failed for {key}: HTTP {response.status}")

    def _signed_request(
        self,
        method: str,
        key: str,
        *,
        body: bytes,
        headers: dict[str, str] | None = None,
    ) -> Request:
        method = method.upper()
        now = datetime.now(timezone.utc)
        amz_date = now.strftime("%Y%m%dT%H%M%SZ")
        date_stamp = now.strftime("%Y%m%d")
        canonical_uri = f"/{quote(self.bucket, safe='')}/{quote_key(key)}"
        host = urlparse(self.endpoint).netloc
        payload_hash = hashlib.sha256(body).hexdigest()
        header_map = {
            "host": host,
            "x-amz-content-sha256": payload_hash,
            "x-amz-date": amz_date,
        }
        for name, value in (headers or {}).items():
            header_map[name.lower()] = str(value).strip()
        signed_header_names = sorted(header_map)
        canonical_headers = "".join(
            f"{name}:{normalize_header_value(header_map[name])}\n"
            for name in signed_header_names
        )
        signed_headers = ";".join(signed_header_names)
        canonical_request = "\n".join(
            [
                method,
                canonical_uri,
                "",
                canonical_headers,
                signed_headers,
                payload_hash,
            ]
        )
        scope = f"{date_stamp}/{self.region}/s3/aws4_request"
        string_to_sign = "\n".join(
            [
                "AWS4-HMAC-SHA256",
                amz_date,
                scope,
                hashlib.sha256(canonical_request.encode("utf-8")).hexdigest(),
            ]
        )
        signature = hmac.new(
            signing_key(self.secret_access_key, date_stamp, self.region, "s3"),
            string_to_sign.encode("utf-8"),
            hashlib.sha256,
        ).hexdigest()
        authorization = (
            "AWS4-HMAC-SHA256 "
            f"Credential={self.access_key_id}/{scope}, "
            f"SignedHeaders={signed_headers}, "
            f"Signature={signature}"
        )
        request_headers = {
            "Authorization": authorization,
            "Host": host,
            "X-Amz-Content-Sha256": payload_hash,
            "X-Amz-Date": amz_date,
        }
        for name, value in (headers or {}).items():
            request_headers[name] = value
        url = f"{self.endpoint}{canonical_uri}"
        return Request(url, data=body if method != "HEAD" else None, headers=request_headers, method=method)


def quote_key(key: str) -> str:
    return "/".join(quote(part, safe="") for part in key.split("/"))


def normalize_header_value(value: str) -> str:
    return " ".join(str(value).strip().split())


def signing_key(secret_key: str, date_stamp: str, region: str, service: str) -> bytes:
    key_date = hmac.new(f"AWS4{secret_key}".encode("utf-8"), date_stamp.encode("utf-8"), hashlib.sha256).digest()
    key_region = hmac.new(key_date, region.encode("utf-8"), hashlib.sha256).digest()
    key_service = hmac.new(key_region, service.encode("utf-8"), hashlib.sha256).digest()
    return hmac.new(key_service, b"aws4_request", hashlib.sha256).digest()


def normalize_base_url(value: str) -> str:
    text = str(value or "").strip()
    if not text:
        raise RuntimeError("base URL is empty")
    return text.rstrip("/") + "/"


def normalize_relative_path(path: str) -> str:
    text = unquote(str(path or "").strip()).replace("\\", "/")
    text = text.split("#", 1)[0].split("?", 1)[0]
    while text.startswith("/"):
        text = text[1:]
    parts: list[str] = []
    for part in text.split("/"):
        if not part or part == ".":
            continue
        if part == "..":
            raise RuntimeError(f"invalid relative path: {path!r}")
        parts.append(part)
    return "/".join(parts)


def make_relative_from_url(url: str, base_url: str) -> str:
    base = normalize_base_url(base_url)
    absolute = urljoin(base, url)
    base_path = urlparse(base).path.rstrip("/") + "/"
    parsed = urlparse(absolute)
    path = parsed.path
    if not path.startswith(base_path):
        return ""
    return normalize_relative_path(path[len(base_path) :])


def parse_index_links(html_text: str) -> list[str]:
    parser = IndexParser()
    parser.feed(html_text)
    return parser.hrefs


def discover_files(
    session: requests.Session,
    upstream_base: str,
    *,
    min_expected_files: int = 10,
) -> list[SdkFile]:
    base = normalize_base_url(upstream_base)
    seen_dirs: set[str] = set()
    files: dict[str, SdkFile] = {}

    def walk(directory_url: str) -> None:
        directory_url = normalize_base_url(directory_url)
        if directory_url in seen_dirs:
            return
        seen_dirs.add(directory_url)
        response = session.get(directory_url, timeout=60)
        response.raise_for_status()
        for href in parse_index_links(response.text):
            if not href or href.startswith("?") or href.startswith("#") or href == "../":
                continue
            absolute = urljoin(directory_url, href)
            relative = make_relative_from_url(absolute, base)
            if not relative:
                continue
            if href.endswith("/") or absolute.endswith("/"):
                walk(absolute)
            else:
                files[relative] = SdkFile(relative, absolute)

    walk(base)
    discovered = sorted(files.values(), key=lambda item: item.relative_path)
    if len(discovered) < min_expected_files:
        raise RuntimeError(
            f"discovered only {len(discovered)} SDK files from {base}; "
            f"expected at least {min_expected_files}"
        )
    return discovered


def load_config(path: str | Path | None) -> R2Config:
    local_config: dict[str, Any] = {}
    config_path = Path(path).expanduser() if path else DEFAULT_LOCAL_CONFIG
    if config_path.exists():
        local_config = json.loads(config_path.read_text(encoding="utf-8"))
        if not isinstance(local_config, dict):
            raise RuntimeError(f"local config is not an object: {config_path}")

    def pick(env_name: str, config_key: str, default: str = "") -> str:
        return str(os.environ.get(env_name, "") or local_config.get(config_key, "") or default).strip()

    config = R2Config(
        r2_account_id=pick("R2_ACCOUNT_ID", "r2_account_id"),
        r2_access_key_id=pick("R2_ACCESS_KEY_ID", "r2_access_key_id"),
        r2_secret_access_key=pick("R2_SECRET_ACCESS_KEY", "r2_secret_access_key"),
        r2_bucket=pick("R2_BUCKET", "r2_bucket"),
        r2_region=pick("R2_REGION", "r2_region", DEFAULT_R2_REGION),
    )
    missing = [
        name
        for name, value in (
            ("R2_ACCOUNT_ID", config.r2_account_id),
            ("R2_ACCESS_KEY_ID", config.r2_access_key_id),
            ("R2_SECRET_ACCESS_KEY", config.r2_secret_access_key),
            ("R2_BUCKET", config.r2_bucket),
        )
        if not value
    ]
    if missing:
        raise RuntimeError(
            "missing R2 credentials/config values: "
            + ", ".join(missing)
            + f". Set env vars or create {config_path}"
        )
    return config


def object_key(relative_path: str, object_prefix: str) -> str:
    prefix = normalize_relative_path(object_prefix)
    rel = normalize_relative_path(relative_path)
    return f"{prefix}/{rel}" if prefix else rel


def sync_file(
    session: requests.Session,
    r2: R2S3Client,
    file_item: SdkFile,
    *,
    object_prefix: str,
    dry_run: bool,
) -> str:
    key = object_key(file_item.relative_path, object_prefix)
    if dry_run:
        return key
    response = session.get(file_item.source_url, timeout=300, stream=True)
    response.raise_for_status()
    body = io.BytesIO()
    for chunk in response.iter_content(chunk_size=1024 * 1024):
        if chunk:
            body.write(chunk)
    content_type = mimetypes.guess_type(file_item.relative_path)[0] or "application/octet-stream"
    r2.put_object(key=key, body=body, content_type=content_type)
    return key


def sync_files(
    session: requests.Session,
    r2: R2S3Client,
    files: list[SdkFile],
    *,
    object_prefix: str,
    dry_run: bool,
) -> list[str]:
    synced: list[str] = []
    for index, file_item in enumerate(files, start=1):
        key = object_key(file_item.relative_path, object_prefix)
        if not dry_run and r2.head_object(key):
            print(f"[{index}/{len(files)}] exists {key}")
            continue
        sync_file(session, r2, file_item, object_prefix=object_prefix, dry_run=dry_run)
        action = "would sync" if dry_run else "synced"
        print(f"[{index}/{len(files)}] {action} {key}")
        synced.append(file_item.relative_path)
    return synced


def verify_public_urls(
    session: requests.Session,
    files: list[SdkFile],
    public_base: str,
) -> VerificationResult:
    base = normalize_base_url(public_base)
    missing: list[str] = []
    for file_item in files:
        url = urljoin(base, quote_key(file_item.relative_path))
        try:
            response = session.head(url, timeout=60, allow_redirects=True)
            if response.status_code >= 400:
                missing.append(file_item.relative_path)
        except Exception:
            missing.append(file_item.relative_path)
    return VerificationResult(total=len(files), missing_r2=[], missing_public=missing)


def verify_r2_objects(
    r2: R2S3Client,
    files: list[SdkFile],
    *,
    object_prefix: str,
) -> list[str]:
    missing: list[str] = []
    for file_item in files:
        if not r2.head_object(object_key(file_item.relative_path, object_prefix)):
            missing.append(file_item.relative_path)
    return missing


def write_manifest(path: Path, files: list[SdkFile], result: VerificationResult) -> None:
    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "upstream_base": DEFAULT_UPSTREAM_BASE,
        "public_base": DEFAULT_PUBLIC_BASE,
        "total": result.total,
        "missing_r2": result.missing_r2,
        "missing_public": result.missing_public,
        "files": [dataclasses.asdict(file_item) for file_item in files],
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, ensure_ascii=False), encoding="utf-8")


def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Mirror the upstream Gyroflow SDK index to Cloudflare R2.")
    parser.add_argument("--upstream-base", default=DEFAULT_UPSTREAM_BASE)
    parser.add_argument("--public-base", default=DEFAULT_PUBLIC_BASE)
    parser.add_argument("--object-prefix", default=DEFAULT_OBJECT_PREFIX)
    parser.add_argument("--config", default="")
    parser.add_argument("--manifest-out", default="_deployment/sdk-r2-mirror-inventory.json")
    parser.add_argument("--min-expected-files", type=int, default=10)
    parser.add_argument("--inventory-only", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--skip-public-verify", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_arg_parser().parse_args(argv)
    session = requests.Session()
    files = discover_files(
        session,
        args.upstream_base,
        min_expected_files=max(0, int(args.min_expected_files)),
    )
    print(f"Discovered {len(files)} upstream SDK files")
    if args.inventory_only:
        result = VerificationResult(total=len(files), missing_r2=[], missing_public=[])
        write_manifest(Path(args.manifest_out), files, result)
        print(f"Wrote inventory manifest: {args.manifest_out}")
        return 0

    config = load_config(args.config or None)
    r2 = R2S3Client(
        account_id=config.r2_account_id,
        access_key_id=config.r2_access_key_id,
        secret_access_key=config.r2_secret_access_key,
        bucket=config.r2_bucket,
        region=config.r2_region,
    )
    sync_files(session, r2, files, object_prefix=args.object_prefix, dry_run=args.dry_run)
    missing_r2 = [] if args.dry_run else verify_r2_objects(r2, files, object_prefix=args.object_prefix)
    missing_public: list[str] = []
    if not args.skip_public_verify:
        missing_public = verify_public_urls(session, files, args.public_base).missing_public
    result = VerificationResult(
        total=len(files),
        missing_r2=missing_r2,
        missing_public=missing_public,
    )
    write_manifest(Path(args.manifest_out), files, result)
    print(f"Verification total={result.total} missing_r2={len(missing_r2)} missing_public={len(missing_public)}")
    if missing_r2:
        print("Missing R2 objects:")
        for item in missing_r2:
            print(f"  {item}")
    if missing_public:
        print("Missing public URLs:")
        for item in missing_public:
            print(f"  {item}")
    return 1 if missing_r2 or missing_public else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
