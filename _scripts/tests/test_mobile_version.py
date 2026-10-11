# SPDX-License-Identifier: GPL-3.0-or-later
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from ios_metadata import load_metadata
from mobile_version import load_version, update_version


class MobileVersionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        deployment = self.root / '_deployment'
        (deployment / 'ios').mkdir(parents=True)
        (deployment / 'mobile-version.json').write_text('{"version":"1.0.0"}')
        (deployment / 'ios/app.json').write_text(json.dumps({'bundle_identifier': 'com.niyien.stabilizer',
            'display_name': 'NiYien', 'build_number': '4', 'minimum_os_version': '15.0'}))

    def test_skipping_versions_updates_ios_from_the_same_source(self):
        self.assertEqual(update_version(self.root, version='1.0.5'), '1.0.5')
        self.assertEqual(load_version(self.root), '1.0.5')
        self.assertEqual(load_metadata(self.root, {})['version'], '1.0.5')

    def test_bump_operations_increment_and_reset_lower_segments(self):
        for bump, expected in [('patch', '1.0.1'), ('minor', '1.1.0'), ('major', '2.0.0')]:
            self.assertEqual(update_version(self.root, bump=bump), expected)

    def test_invalid_or_decreasing_release_does_not_change_the_file(self):
        for version in ['1.0.0', '0.9.9', '1.0.0.1', '1.0.1-dev.2', 'invalid']:
            with self.subTest(version=version), self.assertRaises(ValueError):
                update_version(self.root, version=version)
            self.assertEqual(load_version(self.root), '1.0.0')


if __name__ == '__main__':
    unittest.main()
