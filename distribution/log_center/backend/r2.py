"""Cloudflare R2 client (S3-compatible) — handles download / delete / head
for the global-region feedback bucket.

R2 endpoint follows the convention
``https://<account_id>.r2.cloudflarestorage.com``. Region must be ``auto``.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Optional


@dataclass
class ObjectMeta:
    key: str
    size: int
    etag: str


class R2Client:
    """Thin boto3 S3 wrapper aimed at one bucket."""

    def __init__(
        self,
        account_id: str,
        access_key_id: str,
        secret_access_key: str,
        bucket: str,
        *,
        endpoint_url: Optional[str] = None,
    ):
        # Lazy-import boto3 here so the orchestrator and tests can import
        # this module without boto3 installed (only the actual download /
        # delete code paths require it). Surface a friendly install hint.
        try:
            import boto3  # noqa: F401
            from botocore.client import Config as BotoConfig
        except ImportError as exc:  # pragma: no cover - exercised at runtime only
            raise ImportError(
                "boto3 is required for R2 access; run "
                "`pip install -r distribution/log_center/requirements.txt`"
            ) from exc

        self.account_id = account_id
        self.bucket = bucket
        self._endpoint = endpoint_url or f"https://{account_id}.r2.cloudflarestorage.com"
        # Signature v4 required for R2.
        self._client = boto3.client(
            "s3",
            endpoint_url=self._endpoint,
            aws_access_key_id=access_key_id,
            aws_secret_access_key=secret_access_key,
            region_name="auto",
            # proxies={} explicitly disables boto3's default env-based
            # HTTP_PROXY pickup, matching the bypass policy used by api.py /
            # kv.py / pan123.py.
            config=BotoConfig(
                signature_version="s3v4",
                retries={"max_attempts": 3},
                proxies={},
            ),
        )

    def normalize_key(self, bucket_path: str) -> str:
        """Strip a leading ``<bucket>/`` if present in the stored path.

        Server records ``bucket_path`` as ``feedback/<yyyymmdd>/<id>.zip``
        (the path inside the bucket). If a future record happens to embed
        the bucket prefix, we'll tolerate it here.
        """
        key = bucket_path.lstrip("/")
        prefix = f"{self.bucket}/"
        if key.startswith(prefix):
            key = key[len(prefix):]
        return key

    def download(self, key: str, target_path: Path) -> None:
        """Stream the object to ``target_path``. Parent dirs are created."""
        key = self.normalize_key(key)
        target_path.parent.mkdir(parents=True, exist_ok=True)
        # download_file is the high-level transfer-manager call; handles
        # multipart automatically for large objects.
        self._client.download_file(self.bucket, key, str(target_path))

    def delete(self, key: str) -> None:
        key = self.normalize_key(key)
        self._client.delete_object(Bucket=self.bucket, Key=key)

    def head(self, key: str) -> Optional[ObjectMeta]:
        key = self.normalize_key(key)
        from botocore.exceptions import ClientError  # local import; see __init__
        try:
            resp = self._client.head_object(Bucket=self.bucket, Key=key)
        except ClientError as exc:
            code = (exc.response or {}).get("Error", {}).get("Code")
            if code in ("404", "NoSuchKey", "NotFound"):
                return None
            raise
        return ObjectMeta(
            key=key,
            size=int(resp.get("ContentLength", 0)),
            etag=str(resp.get("ETag", "")).strip('"'),
        )
