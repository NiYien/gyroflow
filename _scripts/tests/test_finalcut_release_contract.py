import hashlib
import json
import subprocess
import sys
import tempfile
import types
import unittest
import zipfile
from pathlib import Path

try:
    import requests  # noqa: F401
except ModuleNotFoundError:
    # The production/release environments install requests. Keep these pure
    # contract tests runnable in Xcode's minimal system Python without network.
    requests_stub = types.ModuleType("requests")
    requests_stub.Session = object
    sys.modules["requests"] = requests_stub

from _scripts import publish_pan123_release as publish
from distribution.control_center.backend import api as control_center_api


ROOT = Path(__file__).resolve().parents[2]
FINALCUT_ASSET = "GyroflowNiyien-FinalCut-macos.zip"
EXPECTED_PLUGIN_ASSETS = (
    "GyroflowNiyien-OpenFX-windows.zip",
    "GyroflowNiyien-Adobe-windows.aex",
    "GyroflowNiyien-OpenFX-macos.zip",
    "GyroflowNiyien-Adobe-macos.zip",
    "GyroflowNiyien-OpenFX-linux.zip",
    FINALCUT_ASSET,
)


class FakeReleaseGithub:
    def download_asset(self, url: str, destination: Path) -> None:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(url.encode("utf-8"))


class FakeArtifactGithub:
    def __init__(self, archive: Path):
        self.archive = archive

    def download_artifact_archive(self, _url: str, destination: Path) -> None:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(self.archive.read_bytes())


def release_source(asset_names: tuple[str, ...]) -> publish.PluginSource:
    return publish.PluginSource(
        mode="release",
        source_ref="v9.9.9",
        display_name="v9.9.9",
        owner="NiYien",
        repo="gyroflow-plugins",
        release={
            "tag_name": "v9.9.9",
            "assets": [
                {
                    "id": index,
                    "name": name,
                    "updated_at": "2026-08-31T00:00:00Z",
                    "browser_download_url": f"https://example.test/{name}",
                }
                for index, name in enumerate(asset_names, start=1)
            ],
        },
    )


def downloaded(name: str, path: Path) -> publish.DownloadedFile:
    payload = path.read_bytes()
    return publish.DownloadedFile(
        logical_path=name,
        local_path=path,
        source="plugin",
        source_tag="v9.9.9",
        size=len(payload),
        sha256=hashlib.sha256(payload).hexdigest(),
    )


class FinalCutReleaseContractTests(unittest.TestCase):
    def test_publish_and_inventory_expect_exactly_six_plugin_assets(self):
        self.assertEqual(publish.PLUGIN_ASSET_NAMES, EXPECTED_PLUGIN_ASSETS)
        self.assertEqual(control_center_api.EXPECTED_PLUGIN_ASSETS, EXPECTED_PLUGIN_ASSETS)
        self.assertEqual(
            publish.EXPECTED_PLUGIN_FILENAMES,
            EXPECTED_PLUGIN_ASSETS + (publish.PLUGIN_MANIFEST_ASSET_NAME,),
        )
        self.assertEqual(
            control_center_api.EXPECTED_PLUGIN_FILENAMES,
            publish.EXPECTED_PLUGIN_FILENAMES,
        )

    def test_release_source_resolves_finalcut_and_rejects_any_missing_asset(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = publish.resolve_plugin_asset_files(
                github=FakeReleaseGithub(),
                temp_root=root,
                plugin_source=release_source(EXPECTED_PLUGIN_ASSETS),
            )
            self.assertEqual(tuple(result), EXPECTED_PLUGIN_ASSETS)
            self.assertTrue(result[FINALCUT_ASSET].is_file())

            with self.assertRaisesRegex(RuntimeError, FINALCUT_ASSET):
                publish.resolve_plugin_asset_files(
                    github=FakeReleaseGithub(),
                    temp_root=root / "missing",
                    plugin_source=release_source(EXPECTED_PLUGIN_ASSETS[:-1]),
                )

    def test_artifact_source_resolves_finalcut_and_rejects_any_missing_asset(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            complete = root / "complete.zip"
            with zipfile.ZipFile(complete, "w") as archive:
                for name in EXPECTED_PLUGIN_ASSETS:
                    archive.writestr(f"payload/{name}", name)

            result = publish.resolve_plugin_assets_from_artifacts(
                github=FakeArtifactGithub(complete),
                temp_root=root / "complete-output",
                run_id=42,
                artifacts=[
                    {
                        "id": 1,
                        "digest": "sha256:complete",
                        "name": "all-plugins",
                        "archive_download_url": "https://example.test/complete.zip",
                    }
                ],
                source_ref="actions-run-42",
            )
            self.assertEqual(tuple(result), EXPECTED_PLUGIN_ASSETS)
            self.assertTrue(result[FINALCUT_ASSET].is_file())

            incomplete = root / "incomplete.zip"
            with zipfile.ZipFile(incomplete, "w") as archive:
                for name in EXPECTED_PLUGIN_ASSETS[:-1]:
                    archive.writestr(f"payload/{name}", name)
            with self.assertRaisesRegex(RuntimeError, FINALCUT_ASSET):
                publish.resolve_plugin_assets_from_artifacts(
                    github=FakeArtifactGithub(incomplete),
                    temp_root=root / "incomplete-output",
                    run_id=43,
                    artifacts=[
                        {
                            "id": 2,
                            "digest": "sha256:incomplete",
                            "name": "all-plugins",
                            "archive_download_url": "https://example.test/incomplete.zip",
                        }
                    ],
                    source_ref="actions-run-43",
                )

    def test_plugin_manifest_is_schema_two_and_requires_exactly_six_assets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = []
            for name in EXPECTED_PLUGIN_ASSETS:
                path = root / name
                path.write_bytes(name.encode("utf-8"))
                files.append(downloaded(name, path))
            source = release_source(EXPECTED_PLUGIN_ASSETS)

            manifest, plugin_tag = publish.build_plugin_manifest(
                plugin_source=source,
                downloaded_plugin=files,
            )
            self.assertEqual(manifest["schema"], 2)
            self.assertEqual(manifest["kind"], "plugin")
            self.assertEqual(
                {entry["path"] for entry in manifest["files"]},
                set(EXPECTED_PLUGIN_ASSETS),
            )
            self.assertTrue(plugin_tag.startswith("plugin-"))

            with self.assertRaisesRegex(RuntimeError, FINALCUT_ASSET):
                publish.build_plugin_manifest(
                    plugin_source=source,
                    downloaded_plugin=files[:-1],
                )

    def test_manifest_keeps_one_plugins_base_for_global_and_cn_routes(self):
        script = r'''
const handler = require('./api/manifest');
const filename = 'GyroflowNiyien-FinalCut-macos.zip';
process.env.NIYIEN_RELEASE_POLICY_JSON = JSON.stringify({
  auto_version: '9.9.9',
  versions: [{
    version: '9.9.9', tag: 'v9.9.9', channels: ['auto', 'manual'],
    plugin_tag: 'plugin-six-assets',
    global_plugins_base: 'https://github.com/NiYien/gyroflow-plugins/releases/latest/download/'
  }]
});
process.env.NIYIEN_LENS_DISABLED = '1';
process.env.NIYIEN_SDK_DISABLED = '1';
delete process.env.NIYIEN_PLUGINS_DISABLED;

function call(country) {
  const req = {
    query: { country, platform: 'macos' },
    headers: { host: 'www.niyien.com', 'x-forwarded-proto': 'https' },
    socket: {}
  };
  const res = {
    setHeader() {},
    status() { return this; },
    json(payload) { this.payload = payload; }
  };
  return handler(req, res).then(() => res.payload);
}

Promise.all([call('CN'), call('US')]).then(([cn, global]) => {
  if (!cn.plugins_base.endsWith('/api/download/content/plugin-six-assets/')) {
    throw new Error(`CN plugins_base=${cn.plugins_base}`);
  }
  if (global.plugins_base !== 'https://github.com/NiYien/gyroflow-plugins/releases/latest/download/') {
    throw new Error(`global plugins_base=${global.plugins_base}`);
  }
  if (!`${cn.plugins_base}${filename}`.endsWith(`/plugin-six-assets/${filename}`)) {
    throw new Error('CN Final Cut filename did not resolve through plugins_base');
  }
  if (!`${global.plugins_base}${filename}`.endsWith(`/latest/download/${filename}`)) {
    throw new Error('Global Final Cut filename did not resolve through plugins_base');
  }
  if ('finalcut_url' in cn || 'finalcut_url' in global) {
    throw new Error('manifest added a Final Cut-specific schema field');
  }
}).catch(error => { console.error(error); process.exit(1); });
'''
        result = subprocess.run(
            ["node", "-e", script],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
