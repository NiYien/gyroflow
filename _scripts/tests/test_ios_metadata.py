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
from package_ios import signing_settings, validate_distribution_toolchain, xcode_toolchain_metadata


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

    def test_distribution_accepts_current_toolchain_and_ios_15_deployment(self):
        build = 'platform IOS\n    minos 15.0\n    sdk 26.0\n'
        validate_distribution_toolchain('2600', '26.0', build)
        validate_distribution_toolchain('2640', '26.4', build)

    def test_xcode_product_build_overrides_xcodes_internal_compiler_build(self):
        developer = self.root / 'Xcode.app/Contents/Developer'
        developer.mkdir(parents=True)
        (developer.parent / 'Info.plist').write_bytes(plistlib.dumps({'DTXcode': '2660', 'DTXcodeBuild': '17F112'}))
        (developer.parent / 'version.plist').write_bytes(plistlib.dumps({'CFBundleShortVersionString': '26.6', 'ProductBuildVersion': '17F113'}))
        metadata = xcode_toolchain_metadata(developer)
        self.assertEqual(metadata['DTXcode'], '2660')
        self.assertEqual(metadata['DTXcodeBuild'], '17F113')
        self.assertEqual(metadata['DTAppStoreToolsBuild'], '17F113')

    def test_xcode_version_encoding_preserves_patch_versions(self):
        developer = self.root / 'Xcode.app/Contents/Developer'
        developer.mkdir(parents=True)
        (developer.parent / 'version.plist').write_bytes(plistlib.dumps({'CFBundleShortVersionString': '26.4.1', 'ProductBuildVersion': '17E202'}))
        self.assertEqual(xcode_toolchain_metadata(developer)['DTXcode'], '2641')

    def test_xcode_product_metadata_must_not_fall_back_to_internal_build(self):
        developer = self.root / 'Xcode.app/Contents/Developer'
        developer.mkdir(parents=True)
        for metadata in [{'CFBundleShortVersionString': '26.6'}, {'CFBundleShortVersionString': 'unknown', 'ProductBuildVersion': '17F113'}]:
            with self.subTest(metadata=metadata):
                (developer.parent / 'version.plist').write_bytes(plistlib.dumps(metadata))
                with self.assertRaises(ValueError):
                    xcode_toolchain_metadata(developer)

    def test_new_packaging_sdk_does_not_hide_an_old_executable(self):
        build = 'platform IOS\n    minos 15.0\n    sdk 18.5\n'
        with self.assertRaises(ValueError):
            validate_distribution_toolchain('2640', '26.4', build)

    def test_distribution_rejects_old_or_unverifiable_toolchains(self):
        for xcode, sdk, build in [
            ('1640', '26.0', 'sdk 26.0\n'),
            (None, '26.0', 'sdk 26.0\n'),
            ('2600', '18.5', 'sdk 26.0\n'),
            ('2600', 'unknown', 'sdk 26.0\n'),
            ('2600', '26.0', 'platform IOS\n'),
            ('2600', '26.0', 'sdk 26.0\nsdk 18.5\n'),
        ]:
            with self.subTest(xcode=xcode, sdk=sdk, build=build), self.assertRaises(ValueError):
                validate_distribution_toolchain(xcode, sdk, build)


if __name__ == '__main__':
    unittest.main()
