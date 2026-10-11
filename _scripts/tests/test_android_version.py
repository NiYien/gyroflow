# SPDX-License-Identifier: GPL-3.0-or-later
import json
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from publish_pan123_release import build_app_packages_metadata, read_android_version_metadata


class AndroidReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.apk = Path(self.temp.name) / "gyroflow-niyien.apk"
        self.metadata = {"schema": 1, "bundle_identifier": "com.niyien.stabilizer",
                         "version": "1.0.152", "version_code": 213000001}

    def write_apk(self, metadata):
        with zipfile.ZipFile(self.apk, "w") as apk:
            apk.writestr("AndroidManifest.xml", b"fixture manifest")
            if metadata is not None:
                apk.writestr("assets/niyien-version.json", json.dumps(metadata))

    def test_publisher_uses_the_built_apk_version(self):
        self.write_apk(self.metadata)
        package = build_app_packages_metadata({self.apk.name: self.apk})["android"]
        self.assertEqual(package["version"], "1.0.152")
        self.assertEqual(package["version_code"], 213000001)
        self.assertEqual(package["package_size"], self.apk.stat().st_size)
        self.assertTrue(package["package_sha256"])

    def test_old_apks_do_not_invent_a_mobile_version(self):
        self.write_apk(None)
        self.assertEqual(read_android_version_metadata(self.apk), {})

    def test_invalid_versions_stop_publication(self):
        for field, value in [("version", "1.6.3.4"), ("version_code", 0),
                             ("version_code", True), ("version_code", "101"),
                             ("version_code", 2_100_000_001), ("schema", 2),
                             ("bundle_identifier", "com.niyien.gyroflow")]:
            with self.subTest(field=field, value=value):
                self.write_apk({**self.metadata, field: value})
                with self.assertRaises(ValueError):
                    read_android_version_metadata(self.apk)


if __name__ == "__main__":
    unittest.main()
