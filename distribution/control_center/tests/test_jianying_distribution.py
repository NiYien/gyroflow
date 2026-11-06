# SPDX-License-Identifier: GPL-3.0-or-later
import json
import shutil
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from _scripts import publish_pan123_release as publish
from distribution.control_center.backend.api import Api


JIAN_YING_ASSET = "GyroflowNiyien-Jianying-windows.zip"


class JianyingDistributionTests(unittest.TestCase):
    def source(self, mode="release"):
        return publish.PluginSource(
            mode=mode, source_ref="v2.2.0" if mode == "release" else "actions-run-123",
            display_name="v2.2.0", owner="NiYien", repo="gyroflow-plugins", run_id=123,
            release={"tag_name": "v2.2.0"} if mode == "release" else None,
        )

    def downloads(self, root, names):
        result = []
        for name in names:
            path = root / name
            path.write_bytes((name + "\n").encode())
            result.append(publish.build_downloaded_file(name, path, "plugin", "v2.2.0"))
        return result

    def test_seventh_package_metadata_comes_from_actual_file(self):
        with tempfile.TemporaryDirectory() as directory:
            downloads = self.downloads(Path(directory), publish.PLUGIN_ASSET_NAMES)
            manifest, tag = publish.build_plugin_manifest(plugin_source=self.source(), downloaded_plugin=downloads)
            actual = next(item for item in downloads if item.logical_path == JIAN_YING_ASSET)
            self.assertEqual(manifest["plugins_packages"][JIAN_YING_ASSET], {"sha256": actual.sha256, "size": actual.size})
            self.assertEqual(Api._plugin_packages_from_manifest(manifest), manifest["plugins_packages"])
            legacy, legacy_tag = publish.build_plugin_manifest(
                plugin_source=self.source(),
                downloaded_plugin=[item for item in downloads if item.logical_path != JIAN_YING_ASSET])
            self.assertNotIn(JIAN_YING_ASSET, legacy["plugins_packages"])
            self.assertNotEqual(tag, legacy_tag)

    def test_existing_required_package_cannot_disappear(self):
        with tempfile.TemporaryDirectory() as directory:
            downloads = self.downloads(Path(directory), publish.PLUGIN_ASSET_NAMES)
            with self.assertRaisesRegex(RuntimeError, "missing: GyroflowNiyien-OpenFX-windows.zip"):
                publish.build_plugin_manifest(plugin_source=self.source(), downloaded_plugin=downloads[1:])
            with self.assertRaisesRegex(RuntimeError, "duplicates"):
                publish.build_plugin_manifest(plugin_source=self.source(), downloaded_plugin=downloads + [downloads[-1]])

    def test_release_resolution_keeps_old_six_package_sources_available(self):
        class Github:
            def download_asset(self, url, destination):
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(url.encode())

        for names in (publish.REQUIRED_PLUGIN_ASSET_NAMES, publish.PLUGIN_ASSET_NAMES):
            with self.subTest(package_count=len(names)), tempfile.TemporaryDirectory() as directory:
                source = self.source()
                source.release["assets"] = [
                    {"name": name, "id": index, "browser_download_url": "test://" + name}
                    for index, name in enumerate(names, 1)
                ]
                resolved = publish.resolve_plugin_asset_files(github=Github(), temp_root=Path(directory), plugin_source=source)
                self.assertEqual(set(resolved), set(names))
                for name, path in resolved.items():
                    self.assertEqual(path.read_bytes(), ("test://" + name).encode())

    def test_real_artifact_resolution_keeps_inner_jianying_zip(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "artifact.zip"
            with zipfile.ZipFile(archive, "w") as output:
                for name in publish.PLUGIN_ASSET_NAMES:
                    output.writestr(name, "contents:" + name)

            class Github:
                def download_artifact_archive(self, url, destination):
                    shutil.copyfile(archive, destination)

            resolved = publish.resolve_plugin_assets_from_artifacts(
                github=Github(), temp_root=root / "work", run_id=123, source_ref="actions-run-123",
                artifacts=[{"id": 1, "name": "GyroflowNiyien-Jianying-windows", "archive_download_url": "test://artifact"}],
            )
            self.assertEqual(set(resolved), set(publish.PLUGIN_ASSET_NAMES))
            self.assertEqual(resolved[JIAN_YING_ASSET].read_bytes(), ("contents:" + JIAN_YING_ASSET).encode())
            with zipfile.ZipFile(archive, "w") as output:
                for name in publish.REQUIRED_PLUGIN_ASSET_NAMES:
                    output.writestr(name, "contents:" + name)
            with self.assertRaisesRegex(RuntimeError, JIAN_YING_ASSET):
                publish.resolve_plugin_assets_from_artifacts(
                    github=Github(), temp_root=root / "missing", run_id=124, source_ref="actions-run-124",
                    artifacts=[{"id": 2, "name": "GyroflowNiyien-Jianying-windows", "archive_download_url": "test://missing"}],
                )

    def test_global_download_source_is_the_hashed_release_or_run(self):
        self.assertEqual(publish.derive_global_plugins_base(self.source()), "https://github.com/NiYien/gyroflow-plugins/releases/download/v2.2.0/")
        self.assertEqual(publish.derive_global_plugins_base(self.source("artifact")), "https://nightly.link/NiYien/gyroflow-plugins/actions/runs/123/")

    def test_optional_platforms_keep_legacy_sources_usable_independently(self):
        for selected in ["GyroflowNiyien-Jianying-windows.zip", "GyroflowNiyien-Jianying-macos.zip"]:
            with self.subTest(selected=selected), tempfile.TemporaryDirectory() as directory:
                names = (*publish.REQUIRED_PLUGIN_ASSET_NAMES, selected)
                downloads = self.downloads(Path(directory), names)
                manifest, _ = publish.build_plugin_manifest(plugin_source=self.source(), downloaded_plugin=downloads)
                self.assertIn(selected, manifest["plugins_packages"])
                other = next(name for name in publish.OPTIONAL_PLUGIN_ASSET_NAMES if name != selected)
                self.assertNotIn(other, manifest["plugins_packages"])

    def test_switching_identity_clears_previous_package_metadata(self):
        info = {JIAN_YING_ASSET: {"sha256": "a" * 64, "size": 123}}
        envs = {
            "NIYIEN_RELEASE_POLICY_JSON": json.dumps({"auto_version": "1.0.0", "versions": [{"version": "1.0.0", "tag": "v1.0.0", "plugins_packages": info}]}),
            "NIYIEN_PLUGINS_PACKAGES_JSON": Api._plugin_packages_env({"plugin_tag": "plugin-new", "plugin_source_ref": "v2.2.0", "plugins_packages": info}),
        }
        api = Api()
        with mock.patch.dict("os.environ", {"PAN123_CLIENT_ID": "", "PAN123_CLIENT_SECRET": "", "PAN123_RELEASES_ROOT_ID": ""}):
            _, current = api._prepare_resources_policy_update({}, vercel_envs=envs, plugin_mode="release", plugin_release_tag="v2.2.0", plugin_dir_tag="plugin-new")
            self.assertEqual(json.loads(current)["versions"][0]["plugins_packages"], info)
            _, switched = api._prepare_resources_policy_update({}, vercel_envs=envs, plugin_mode="release", plugin_release_tag="v2.1.2", plugin_dir_tag="plugin-old")
            self.assertEqual(json.loads(switched)["versions"][0]["plugins_packages"], {})

    def test_app_only_publish_and_resource_hide_keep_metadata_with_identity(self):
        info = {JIAN_YING_ASSET: {"sha256": "b" * 64, "size": 456}}
        versions = [{"version": "1.0.0", "tag": "v1.0.0", "plugin_tag": "plugin-new", "plugins_packages": info}]
        Api._upsert_version_entry(versions, "1.0.1", "v1.0.1", "", False, ["auto"])
        self.assertEqual(versions[0]["plugins_packages"], info)
        policy = {"auto_version": "1.0.1", "versions": versions}
        envs = Api._apply_hide_resource_scope(policy, versions[0], {"plugin"}, False)
        self.assertTrue(all("plugins_packages" not in entry for entry in versions))
        self.assertEqual(envs["NIYIEN_PLUGINS_PACKAGES_JSON"], "")

    def test_promoted_entry_only_inherits_matching_env_packages_and_route(self):
        info = {JIAN_YING_ASSET: {"sha256": "e" * 64, "size": 456}}
        base = "https://github.com/NiYien/gyroflow-plugins/releases/download/v2.2.0/"
        for recorded_ref in ["v2.2.0", "v2.1.2"]:
            policy = {"auto_version": "1.0.1", "versions": [{"version": "1.0.1", "tag": "v1.0.1"}]}
            envs = {"NIYIEN_PLUGIN_RELEASE_TAG": "plugin-new", "NIYIEN_PLUGINS_SOURCE_MODE": "release", "NIYIEN_PLUGINS_TAG": "v2.2.0",
                    "NIYIEN_PLUGINS_PACKAGES_JSON": Api._plugin_packages_env({"plugin_tag": "plugin-new", "plugin_source_ref": recorded_ref,
                        "global_plugins_base": base, "plugins_packages": info})}
            Api._apply_hide_resource_scope(policy, None, set(), True, envs)
            promoted = policy["versions"][0]
            if recorded_ref == "v2.2.0":
                self.assertEqual(promoted["plugins_packages"], info)
                self.assertEqual(promoted["global_plugins_base"], base)
            else:
                self.assertNotIn("plugins_packages", promoted)


if __name__ == "__main__":
    unittest.main()
