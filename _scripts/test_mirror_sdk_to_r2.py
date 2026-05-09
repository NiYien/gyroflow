import io
import unittest
from unittest import mock

from _scripts import mirror_sdk_to_r2 as mirror


ROOT_INDEX = b"""
<html><body>
<a href="../">Parent Directory</a>
<a href="AdobeSDK.zip">AdobeSDK.zip</a>
<a href="/sdk/shaderc_shared.dll">shaderc_shared.dll</a>
<a href="v1.5.1/">v1.5.1/</a>
</body></html>
"""

NESTED_INDEX = b"""
<html><body>
<a href="../">Parent Directory</a>
<a href="RED_SDK_Windows.tar.gz">RED_SDK_Windows.tar.gz</a>
</body></html>
"""


class FakeResponse:
    def __init__(self, body=b"", status=200):
        self.body = body
        self.text = body.decode("utf-8")
        self.status_code = status
        self.headers = {"Content-Length": str(len(body))}

    def raise_for_status(self):
        if self.status_code >= 400:
            raise RuntimeError(f"HTTP {self.status_code}")

    def iter_content(self, chunk_size=1024 * 1024):
        yield self.body


class FakeSession:
    def __init__(self):
        self.get_urls = []
        self.head_urls = []

    def get(self, url, **kwargs):
        self.get_urls.append(url)
        if url.endswith("/sdk/"):
            return FakeResponse(ROOT_INDEX)
        if url.endswith("/sdk/v1.5.1/"):
            return FakeResponse(NESTED_INDEX)
        return FakeResponse(b"data")

    def head(self, url, **kwargs):
        self.head_urls.append(url)
        return FakeResponse()


class MirrorSdkToR2Tests(unittest.TestCase):
    def test_inventory_recurses_and_preserves_relative_paths(self):
        session = FakeSession()

        files = mirror.discover_files(
            session,
            "https://api.gyroflow.xyz/sdk/",
            min_expected_files=1,
        )

        self.assertEqual(
            [item.relative_path for item in files],
            [
                "AdobeSDK.zip",
                "shaderc_shared.dll",
                "v1.5.1/RED_SDK_Windows.tar.gz",
            ],
        )

    def test_verify_reports_missing_niyien_paths(self):
        session = FakeSession()
        files = [
            mirror.SdkFile("AdobeSDK.zip", "https://api.gyroflow.xyz/sdk/AdobeSDK.zip"),
            mirror.SdkFile(
                "v1.5.1/RED_SDK_Windows.tar.gz",
                "https://api.gyroflow.xyz/sdk/v1.5.1/RED_SDK_Windows.tar.gz",
            ),
        ]

        def fake_head(url, **kwargs):
            if url.endswith("/v1.5.1/RED_SDK_Windows.tar.gz"):
                return FakeResponse(status=404)
            return FakeResponse(status=200)

        session.head = fake_head
        result = mirror.verify_public_urls(
            session,
            files,
            "https://www.niyien.com/api/sdk/",
        )

        self.assertEqual(result.total, 2)
        self.assertEqual(result.missing_public, ["v1.5.1/RED_SDK_Windows.tar.gz"])

    def test_sync_uses_env_or_local_config_for_r2_credentials(self):
        with mock.patch.dict(
            "os.environ",
            {
                "R2_ACCOUNT_ID": "account",
                "R2_ACCESS_KEY_ID": "access",
                "R2_SECRET_ACCESS_KEY": "secret",
                "R2_BUCKET": "bucket",
            },
            clear=True,
        ):
            config = mirror.load_config(None)

        self.assertEqual(config.r2_account_id, "account")
        self.assertEqual(config.r2_bucket, "bucket")

    def test_upload_preserves_sdk_prefix(self):
        session = FakeSession()
        s3 = mirror.R2S3Client(
            account_id="account",
            access_key_id="access",
            secret_access_key="secret",
            bucket="bucket",
        )
        file_item = mirror.SdkFile(
            "v1.5.1/RED_SDK_Windows.tar.gz",
            "https://api.gyroflow.xyz/sdk/v1.5.1/RED_SDK_Windows.tar.gz",
        )

        with mock.patch.object(s3, "put_object") as put_object:
            mirror.sync_file(
                session,
                s3,
                file_item,
                object_prefix="sdk/",
                dry_run=False,
            )

        put_object.assert_called_once()
        self.assertEqual(put_object.call_args.kwargs["key"], "sdk/v1.5.1/RED_SDK_Windows.tar.gz")
        self.assertIsInstance(put_object.call_args.kwargs["body"], io.BytesIO)


if __name__ == "__main__":
    unittest.main()
