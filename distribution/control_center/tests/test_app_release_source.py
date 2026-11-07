# SPDX-License-Identifier: GPL-3.0-or-later
import hashlib
import shutil
import tempfile
import unittest
import zipfile
from pathlib import Path

from _scripts import publish_pan123_release as publish


class ReleaseGitHub:
    def __init__(self, root):
        self.root = root
        self.release_calls = []
        self.downloads = []
        self.assets = []
        for index, name in enumerate(publish.REQUIRED_APP_ASSET_NAMES, 1):
            path = root / name
            if name == "gyroflow-niyien-windows64.zip":
                with zipfile.ZipFile(path, "w") as archive:
                    archive.writestr("Gyroflow.exe", b"release executable")
            else:
                path.write_bytes(("release asset " + name).encode())
            self.assets.append({
                "id": index,
                "name": name,
                "updated_at": "2026-11-07T08:10:10Z",
                "size": path.stat().st_size,
                "digest": "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest(),
                "browser_download_url": name,
            })

    def get_release(self, owner, repo, tag):
        self.release_calls.append((owner, repo, tag))
        return {"tag_name": tag, "assets": self.assets}

    def download_asset(self, url, destination):
        self.downloads.append(url)
        shutil.copyfile(self.root / url, destination)


class AppReleaseSourceTests(unittest.TestCase):
    def resolve(self, github, root):
        return publish.resolve_app_asset_files(
            github=github,
            app_tag="v1.7.0",
            temp_root=root,
            app_source_mode="release",
            app_owner="NiYien",
            app_repo="gyroflow",
            app_run_id=0,
        )

    def test_selected_release_replaces_local_placeholder_and_reuses_verified_cache(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            github = ReleaseGitHub(root)
            staging = root / "staging"
            (staging / "app").mkdir(parents=True)
            (staging / "app" / "gyroflow-niyien-windows64.zip").write_bytes(b"local placeholder")
            resolved = self.resolve(github, staging)
            self.assertEqual(github.release_calls, [("NiYien", "gyroflow", "v1.7.0")])
            self.assertEqual(set(resolved), set(publish.REQUIRED_APP_ASSET_NAMES))
            for name, path in resolved.items():
                self.assertEqual(path.read_bytes(), (root / name).read_bytes())
            with zipfile.ZipFile(resolved["gyroflow-niyien-windows64.zip"]) as archive:
                self.assertEqual(archive.read("Gyroflow.exe"), b"release executable")
            download_count = len(github.downloads)
            self.assertEqual(self.resolve(github, staging), resolved)
            self.assertEqual(len(github.downloads), download_count)

    def test_missing_release_zip_fails_even_when_same_named_local_file_exists(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            github = ReleaseGitHub(root)
            github.assets = [a for a in github.assets if a["name"] != "gyroflow-niyien-windows64.zip"]
            with self.assertRaisesRegex(RuntimeError, "Missing required app assets in GitHub release v1.7.0"):
                self.resolve(github, root / "staging")
            self.assertEqual(github.downloads, [])

    def test_incorrect_github_size_or_sha256_stops_resolution(self):
        for field, value, error in (("size", 1, "size mismatch"), ("digest", "sha256:" + "0" * 64, "SHA256 mismatch")):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                github = ReleaseGitHub(root)
                github.assets[0][field] = value
                with self.assertRaisesRegex(RuntimeError, error):
                    self.resolve(github, root / "staging")


if __name__ == "__main__":
    unittest.main()
