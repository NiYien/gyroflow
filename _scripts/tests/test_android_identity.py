# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify Android identity across the manifest, Java classes and JNI exports."""
import json
from pathlib import Path
import re
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
ANDROID = ROOT / "_deployment/android"
NS = "{http://schemas.android.com/apk/res/android}"


class AndroidIdentityTests(unittest.TestCase):
    def setUp(self):
        self.metadata = json.loads((ANDROID / "app.json").read_text())
        self.manifest = ET.parse(ANDROID / "AndroidManifest.xml").getroot()
        self.package = self.metadata["bundle_identifier"]
        self.java_dir = ANDROID / "src" / self.package.replace(".", "/")
        self.activity = (self.java_dir / "MainActivity.java").read_text()

    def test_mobile_identity_and_display_version_match(self):
        ios = json.loads((ROOT / "_deployment/ios/app.json").read_text())
        for field in ["bundle_identifier", "display_name"]:
            self.assertEqual(self.metadata[field], ios[field])
        self.assertNotIn("version", self.metadata)
        self.assertNotIn("version", ios)
        self.assertEqual(self.manifest.attrib["package"], self.package)
        self.assertNotIn(NS + "versionName", self.manifest.attrib)
        self.assertEqual(self.manifest.attrib[NS + "versionCode"], self.metadata["version_code"])
        for element in [self.manifest.find("application"), self.manifest.find("application/activity")]:
            self.assertEqual(element.attrib[NS + "label"], self.metadata["display_name"])

    def test_java_sources_and_native_callbacks_resolve(self):
        self.assertEqual(self.manifest.find("application/activity").attrib[NS + "name"], self.package + ".MainActivity")
        for path in self.java_dir.glob("*.java"):
            self.assertIn("package " + self.package + ";", path.read_text())
        native_methods = set(re.findall(r"public static native \w+ (\w+)\(", self.activity))
        self.assertTrue(native_methods)
        rust = "\n".join((ROOT / name).read_text() for name in ["src/util.rs", "src/niyien_device/mobile_backend.rs"])
        prefix = "Java_" + self.package.replace(".", "_") + "_MainActivity_"
        exported = set(re.findall(re.escape(prefix) + r"(\w+)", rust))
        self.assertEqual(native_methods, exported)
        self.assertNotIn("Java_com_niyien_gyroflow_", rust)
        for name in ["src/util.rs", "src/niyien_device/mobile_backend.rs"]:
            self.assertIn('env.new_string("' + self.package + '.MainActivity")', (ROOT / name).read_text())

    def test_provider_usb_action_and_launcher_match_package(self):
        provider = self.manifest.find("application/provider")
        authority = provider.attrib[NS + "authorities"]
        self.assertEqual(authority, self.package + ".updateprovider")
        self.assertIn('"' + authority + '"', self.activity)
        self.assertIn('"' + self.package + '.USB_PERMISSION"', self.activity)
        script = (ROOT / "_scripts/android.just").read_text()
        self.assertIn(self.package + "/" + self.package + ".MainActivity", script)
        self.assertNotIn("com.niyien.gyroflow", script)

    def test_intermediate_and_final_android_targets_agree(self):
        cargo = (ROOT / "Cargo.toml").read_text()
        self.assertIn('package = "' + self.package + '"', cargo)
        target = int(re.search(r"target_sdk_version\s*=\s*(\d+)", cargo).group(1))
        script = (ROOT / "_scripts/android.just").read_text()
        self.assertGreaterEqual(target, 36)
        self.assertIn('"android-target-sdk-version": "' + str(target) + '"', script)
        self.assertIn("--android-platform android-" + str(target), script)


if __name__ == "__main__":
    unittest.main()
