# SPDX-License-Identifier: GPL-3.0-or-later
import copy
from datetime import datetime, timedelta
import hashlib
import json
import os
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from ios_metadata import load_metadata
from package_ios import signing_settings


class IOSIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.file = self.root / '_deployment/ios/app.json'
        self.file.parent.mkdir(parents=True)
        self.metadata = {'bundle_identifier': 'com.niyien.stabilizer', 'display_name': 'NiYien',
                         'version': '1.0.0', 'build_number': '1', 'minimum_os_version': '15.0'}
        self.file.write_text(json.dumps(self.metadata))

    def test_build_counter_does_not_change_marketing_version(self):
        result = load_metadata(self.root, {'NIYIEN_IOS_BUILD_NUMBER': '18'})
        self.assertEqual(result['version'], '1.0.0')
        self.assertEqual(result['build_number'], '18')

    def test_rejects_previous_four_segment_version(self):
        self.metadata['version'] = '1.6.3.1'
        self.file.write_text(json.dumps(self.metadata))
        with self.assertRaises(ValueError):
            load_metadata(self.root, {})

    def test_rejects_invalid_build_numbers(self):
        for value in ['0', '-1', '1.0.0.1', '10000', 'nightly', '']:
            with self.subTest(value=value), self.assertRaises(ValueError):
                load_metadata(self.root, {'NIYIEN_IOS_BUILD_NUMBER': value})

    def test_signing_requires_matching_bundle_certificate_and_kind(self):
        profile = self.root / 'test.mobileprovision'
        profile.touch()
        certificate = b'fixture certificate'
        granted = {'TeamIdentifier': ['TESTTEAM'], 'DeveloperCertificates': [certificate],
                   'ExpirationDate': datetime.now() + timedelta(days=1),
                   'Entitlements': {'application-identifier': 'TESTTEAM.com.niyien.stabilizer', 'get-task-allow': True}}
        variants = [('valid', None), ('bundle', 'TESTTEAM.com.niyien.gyroflow'), ('wildcard', 'TESTTEAM.*'),
                    ('distribution', False), ('expired', datetime.now() - timedelta(days=1)), ('certificate', b'other certificate')]
        for name, value in variants:
            data = copy.deepcopy(granted)
            if name in ('bundle', 'wildcard'):
                data['Entitlements']['application-identifier'] = value
            elif name == 'distribution':
                data['Entitlements']['get-task-allow'] = value
            elif name == 'expired':
                data['ExpirationDate'] = value
            elif name == 'certificate':
                data['DeveloperCertificates'] = [value]
            with self.subTest(name=name), patch.dict(os.environ, {'PROVISIONING_PROFILE': str(profile), 'SIGN_KEY': hashlib.sha1(certificate).hexdigest()}), patch('package_ios.subprocess.check_output', return_value=plistlib.dumps(data)):
                if name == 'valid':
                    self.assertEqual(signing_settings('development', self.metadata)[0], profile)
                else:
                    with self.assertRaises(ValueError):
                        signing_settings('development', self.metadata)

    def test_distribution_excludes_ad_hoc_and_enterprise_profiles(self):
        profile = self.root / 'distribution.mobileprovision'
        profile.touch()
        certificate = b'fixture certificate'
        for extra in [{'ProvisionedDevices': ['test-device']}, {'ProvisionsAllDevices': True}]:
            data = {'TeamIdentifier': ['TESTTEAM'], 'DeveloperCertificates': [certificate],
                    'ExpirationDate': datetime.now() + timedelta(days=1),
                    'Entitlements': {'application-identifier': 'TESTTEAM.com.niyien.stabilizer', 'get-task-allow': False}, **extra}
            with self.subTest(extra=extra), patch.dict(os.environ, {'PROVISIONING_PROFILE': str(profile), 'SIGN_KEY': hashlib.sha1(certificate).hexdigest()}), patch('package_ios.subprocess.check_output', return_value=plistlib.dumps(data)), self.assertRaises(ValueError):
                signing_settings('distribution', self.metadata)


if __name__ == '__main__':
    unittest.main()
