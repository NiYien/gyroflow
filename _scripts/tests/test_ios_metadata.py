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
from ios_metadata import allocate_build, load_metadata
from package_ios import signing_settings, validate_distribution_toolchain, xcode_toolchain_metadata


class IOSIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.file = self.root / '_deployment/ios/app.json'
        self.file.parent.mkdir(parents=True)
        self.metadata = {'bundle_identifier': 'com.niyien.stabilizer', 'display_name': 'NiYien',
                         'build_number': '1', 'minimum_os_version': '15.0'}
        self.file.write_text(json.dumps(self.metadata))
        self.release_file = self.root / '_deployment/mobile-version.json'
        self.release_file.write_text(json.dumps({'version': '1.0.0'}))

    def test_build_counter_does_not_change_marketing_version(self):
        result = load_metadata(self.root, {'NIYIEN_IOS_BUILD_NUMBER': '18'})
        self.assertEqual(result['version'], '1.0.0')
        self.assertEqual(result['build_number'], '18')

    def test_rejects_previous_four_segment_version(self):
        self.release_file.write_text(json.dumps({'version': '1.6.3.1'}))
        with self.assertRaises(ValueError):
            load_metadata(self.root, {})

    def test_rejects_invalid_build_numbers(self):
        for value in ['0', '-1', '1.0.0.1', '10000', '1.100', '1.0.100', 'nightly', '']:
            with self.subTest(value=value), self.assertRaises(ValueError):
                load_metadata(self.root, {'NIYIEN_IOS_BUILD_NUMBER': value})

    def test_packaging_reserves_a_new_number_and_reuses_it(self):
        first = allocate_build(self.root, {})
        second = allocate_build(self.root, {})
        self.assertEqual(first['build_number'], '2')
        self.assertEqual(second['build_number'], '3')
        self.assertEqual(load_metadata(self.root, {})['build_number'], '3')
        self.assertEqual(second['version'], '1.0.0')

    def test_ci_runs_and_attempts_do_not_reuse_build_numbers(self):
        first = allocate_build(self.root, {'GITHUB_ACTIONS': 'true', 'GITHUB_RUN_NUMBER': '151', 'GITHUB_RUN_ATTEMPT': '1'})
        retry = allocate_build(self.root, {'GITHUB_ACTIONS': 'true', 'GITHUB_RUN_NUMBER': '151', 'GITHUB_RUN_ATTEMPT': '2'})
        following = allocate_build(self.root, {'GITHUB_ACTIONS': 'true', 'GITHUB_RUN_NUMBER': '152', 'GITHUB_RUN_ATTEMPT': '1'})
        self.assertEqual(first['build_number'], '152.1')
        self.assertEqual(retry['build_number'], '152.2')
        self.assertEqual(following['build_number'], '153.1')
        self.assertEqual(load_metadata(self.root, {})['build_number'], '153.1')
        self.assertEqual(following['version'], '1.0.0')

    def test_dotted_counter_carries_and_orders_numerically(self):
        allocate_build(self.root, {'NIYIEN_IOS_BUILD_NUMBER': '5.99.99'})
        self.assertEqual(allocate_build(self.root, {})['build_number'], '6.0.0')
        self.assertEqual(allocate_build(self.root, {'NIYIEN_IOS_BUILD_NUMBER': '10.1'})['build_number'], '10.1')

    def test_fresh_ci_runner_uses_run_number_without_local_state(self):
        result = allocate_build(self.root, {'GITHUB_ACTIONS': 'true', 'GITHUB_RUN_NUMBER': '1'})
        self.assertEqual(result['build_number'], '2.1')

    def test_invalid_ci_counter_does_not_reserve_a_number(self):
        for run, attempt in [('0', '1'), ('1', '100'), ('invalid', '1'), ('9999', '1')]:
            with self.subTest(run=run, attempt=attempt), self.assertRaises(ValueError):
                allocate_build(self.root, {'GITHUB_ACTIONS': 'true', 'GITHUB_RUN_NUMBER': run, 'GITHUB_RUN_ATTEMPT': attempt})
        self.assertEqual(load_metadata(self.root, {})['build_number'], '1')

    def test_store_build_override_advances_the_local_counter(self):
        self.assertEqual(allocate_build(self.root, {'NIYIEN_IOS_BUILD_NUMBER': '18'})['build_number'], '18')
        self.assertEqual(allocate_build(self.root, {})['build_number'], '19')
        with self.assertRaises(ValueError):
            allocate_build(self.root, {'NIYIEN_IOS_BUILD_NUMBER': '18'})

    def test_updated_store_floor_advances_existing_counter(self):
        allocate_build(self.root, {})
        self.metadata['build_number'] = '20'
        self.file.write_text(json.dumps(self.metadata))
        self.assertEqual(allocate_build(self.root, {})['build_number'], '21')

    def test_shared_release_can_skip_versions(self):
        allocate_build(self.root, {})
        self.release_file.write_text(json.dumps({'version': '1.0.5'}))
        result = allocate_build(self.root, {})
        self.assertEqual(result['version'], '1.0.5')
        self.assertEqual(result['build_number'], '2')

    def test_build_allocation_refuses_concurrent_writes(self):
        lock = self.root / 'target/ios-version/build-number.lock'
        lock.parent.mkdir(parents=True)
        lock.touch()
        with self.assertRaisesRegex(ValueError, 'Another iOS build'):
            allocate_build(self.root, {})

    def test_signing_requires_matching_bundle_certificate_and_kind(self):
        profile = self.root / 'test.mobileprovision'
        profile.touch()
        certificate = b'fixture certificate'
        granted = {'Platform': ['iOS'], 'TeamIdentifier': ['TESTTEAM'], 'DeveloperCertificates': [certificate],
                   'ExpirationDate': datetime.now() + timedelta(days=1),
                   'Entitlements': {'application-identifier': 'TESTTEAM.com.niyien.stabilizer', 'get-task-allow': True}}
        variants = [('valid', None), ('bundle', 'TESTTEAM.com.niyien.gyroflow'), ('wildcard', 'TESTTEAM.*'),
                    ('distribution', False), ('expired', datetime.now() - timedelta(days=1)), ('certificate', b'other certificate'),
                    ('platform', ['OSX'])]
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
            elif name == 'platform':
                data['Platform'] = value
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
            data = {'Platform': ['iOS'], 'TeamIdentifier': ['TESTTEAM'], 'DeveloperCertificates': [certificate],
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
