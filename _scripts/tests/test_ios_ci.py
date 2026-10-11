# SPDX-License-Identifier: GPL-3.0-or-later
import base64
import hashlib
import json
import os
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest.mock import patch
import subprocess
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from ios_ci import API_CONFIGURATION, check_config, distribution_identities, prepare_artifact, select_profile, upload_account, upload_requested, write_github_values
from ios_metadata import allocate_build, load_metadata


class IOSWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / '_deployment/ios').mkdir(parents=True)
        (self.root / '_deployment/mobile-version.json').write_text('{"version":"1.0.5"}')
        (self.root / '_deployment/ios/app.json').write_text(json.dumps({'display_name': 'NiYien',
            'bundle_identifier': 'com.niyien.stabilizer', 'build_number': '4', 'minimum_os_version': '15.0'}))

    def test_missing_configuration_reports_names_without_credentials(self):
        environment = {name: 'private fixture credential' for name in API_CONFIGURATION}
        environment['IOS_UPLOAD_TO_CONNECT'] = 'true'
        environment.update(MACOS_CERTIFICATES='fixture', MACOS_CERTIFICATE_PWD='fixture')
        environment.pop('APPSTORE_API_PRIVATE_KEY')
        with self.assertRaisesRegex(ValueError, 'APPSTORE_API_PRIVATE_KEY') as caught:
            check_config(environment, self.root)
        self.assertNotIn('private fixture credential', str(caught.exception))

    def test_workflow_uses_the_mobile_bundle_and_rejects_suffix_tags(self):
        environment = {name: 'fixture' for name in API_CONFIGURATION}
        environment['IOS_UPLOAD_TO_CONNECT'] = 'true'
        environment.update(MACOS_CERTIFICATES='fixture', MACOS_CERTIFICATE_PWD='fixture')
        environment['GITHUB_REF'] = 'refs/tags/v1.6.4'
        self.assertEqual(check_config(environment, self.root)['bundle_identifier'], 'com.niyien.stabilizer')
        environment['GITHUB_REF'] = 'refs/tags/v1.6.4-ni.1'
        with self.assertRaises(ValueError):
            check_config(environment, self.root)

    def shared_config(self):
        return {'IOS_UPLOAD_TO_CONNECT': 'true', 'MACOS_CERTIFICATES': 'shared certificate', 'MACOS_CERTIFICATE_PWD': 'shared password',
                'MACOS_ACCOUNT_USER': 'fixture@example.com', 'MACOS_ACCOUNT_PASS': 'private fixture password',
                'IOS_PROVISIONING_PROFILE': base64.b64encode(b'fixture profile').decode()}

    def test_unchecked_build_does_not_require_signing_or_upload_credentials(self):
        result = check_config({}, self.root)
        self.assertEqual(result['upload_enabled'], 'false')
        self.assertEqual(result['upload_mode'], 'none')
        self.assertEqual(result['profile_mode'], 'none')

    def test_false_or_missing_input_cannot_enable_upload(self):
        self.assertFalse(upload_requested({'IOS_UPLOAD_TO_CONNECT': 'false'}, self.root))
        self.assertFalse(upload_requested({}, self.root))
        with self.assertRaises(ValueError):
            upload_requested({'IOS_UPLOAD_TO_CONNECT': '1'}, self.root)

    def test_unchecked_account_upload_cannot_start_the_uploader(self):
        environment = self.shared_config()
        environment['IOS_UPLOAD_TO_CONNECT'] = 'false'
        uploader = unittest.mock.Mock()
        with self.assertRaisesRegex(ValueError, 'disabled'):
            upload_account(environment, self.root, uploader)
        uploader.assert_not_called()

    def test_existing_mac_credentials_only_need_the_ios_profile(self):
        result = check_config(self.shared_config(), self.root)
        self.assertEqual(result['upload_mode'], 'account')
        self.assertEqual(result['profile_mode'], 'embedded')

    def test_existing_api_aliases_do_not_need_duplicate_keys(self):
        environment = self.shared_config()
        environment.pop('IOS_PROVISIONING_PROFILE')
        environment.update({alias: 'existing fixture' for alias in API_CONFIGURATION.values()})
        result = check_config(environment, self.root)
        self.assertEqual(result['upload_mode'], 'account')
        self.assertEqual(result['profile_mode'], 'api')
        environment.pop('MACOS_ACCOUNT_USER')
        environment.pop('MACOS_ACCOUNT_PASS')
        self.assertEqual(check_config(environment, self.root)['upload_mode'], 'api')

    def test_dedicated_ios_certificate_can_reuse_shared_password(self):
        environment = self.shared_config()
        environment.pop('MACOS_CERTIFICATES')
        environment['IOS_CERTIFICATES'] = 'ios certificate override'
        self.assertEqual(check_config(environment, self.root)['upload_mode'], 'account')

    def test_profile_selection_skips_invalid_profiles_and_preserves_uuid_case(self):
        identifiers = ['11111111-1111-1111-1111-111111111111', 'AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA']
        directory = self.root / 'profiles'
        directory.mkdir()
        for identifier in identifiers:
            (directory / (identifier + '.mobileprovision')).touch()
        fingerprint = 'A' * 40
        previous = os.environ.get('PROVISIONING_PROFILE')
        def validate(kind, metadata):
            self.assertEqual(kind, 'distribution')
            self.assertEqual(metadata['bundle_identifier'], 'com.niyien.stabilizer')
            path = Path(os.environ['PROVISIONING_PROFILE'])
            if path.stem == identifiers[0]:
                raise ValueError('expired fixture profile')
            return path, fingerprint, {}
        result = select_profile({'IOS_PROVISIONING_PROFILES': json.dumps([{'udid': value} for value in identifiers]),
            'IOS_SIGNING_FINGERPRINT': fingerprint}, self.root, directory, validate, [fingerprint])
        self.assertEqual(Path(result['PROVISIONING_PROFILE']).stem, identifiers[1])
        self.assertEqual(result['SIGN_KEY'], fingerprint)
        self.assertEqual(os.environ.get('PROVISIONING_PROFILE'), previous)

    def test_embedded_profile_and_shared_private_key_are_selected(self):
        fingerprint = 'B' * 40
        environment = self.shared_config()
        environment.update(IOS_PROFILE_MODE='embedded', RUNNER_TEMP=str(self.root / 'runner temp'))
        def validate(kind, metadata):
            self.assertEqual(os.environ['SIGN_KEY'], fingerprint)
            path = Path(os.environ['PROVISIONING_PROFILE'])
            self.assertEqual(path.read_bytes(), b'fixture profile')
            return path, fingerprint, {}
        result = select_profile(environment, self.root, validator=validate, identities=[fingerprint])
        self.assertEqual(result['SIGN_KEY'], fingerprint)

    def test_developer_id_alone_cannot_be_used_for_ios(self):
        environment = self.shared_config()
        environment.update(IOS_PROFILE_MODE='embedded', RUNNER_TEMP=str(self.root / 'runner temp'))
        with self.assertRaisesRegex(ValueError, 'Apple Distribution private key'):
            select_profile(environment, self.root, identities=[])

    def test_only_distribution_private_identities_are_candidates(self):
        response = f'1) {"A" * 40} "Developer ID Application: Fixture"\n2) {"B" * 40} "Apple Distribution: Fixture"\n'
        with patch('ios_ci.subprocess.check_output', return_value=response):
            self.assertEqual(distribution_identities(), ['B' * 40])

    def test_empty_profile_result_cannot_fall_back_to_a_local_profile(self):
        with self.assertRaisesRegex(ValueError, 'No IOS_APP_STORE'):
            select_profile({}, self.root)

    def make_ipa(self):
        metadata = allocate_build(self.root, {'GITHUB_ACTIONS': 'true', 'GITHUB_RUN_NUMBER': '1'})
        binaries = self.root / '_deployment/_binaries'
        (binaries / 'ios').mkdir(parents=True)
        ipa = binaries / 'NiYien-fixture.ipa'
        info = {'CFBundleIdentifier': metadata['bundle_identifier'], 'CFBundleShortVersionString': metadata['version'],
                'CFBundleVersion': metadata['build_number']}
        with zipfile.ZipFile(ipa, 'w') as archive:
            archive.writestr('Payload/NiYien.app/Info.plist', plistlib.dumps(info))
        receipt = {**metadata, 'ipa': str(ipa), 'sha256': hashlib.sha256(ipa.read_bytes()).hexdigest(), 'signing': 'distribution'}
        path = binaries / 'ios/build-receipt.json'
        path.write_text(json.dumps(receipt))
        return path, ipa, receipt

    def test_upload_selects_the_verified_distribution_ipa(self):
        _, ipa, _ = self.make_ipa()
        self.assertEqual(prepare_artifact(self.root)['ipa_path'], str(ipa.resolve()))

    def test_compile_only_accepts_unsigned_artifact_but_upload_rejects_it(self):
        path, _, receipt = self.make_ipa()
        receipt['signing'] = 'unsigned'
        path.write_text(json.dumps(receipt))
        self.assertTrue(prepare_artifact(self.root, require_distribution=False)['ipa_path'])
        with self.assertRaisesRegex(ValueError, 'distribution-signed'):
            prepare_artifact(self.root)

    def test_stale_unsigned_or_modified_ipa_cannot_be_uploaded(self):
        path, ipa, receipt = self.make_ipa()
        for key, value in [('signing', 'unsigned'), ('version', '1.0.0'), ('build_number', '4'), ('sha256', 'wrong')]:
            path.write_text(json.dumps({**receipt, key: value}))
            with self.subTest(key=key), self.assertRaises(ValueError):
                prepare_artifact(self.root)
        path.write_text(json.dumps(receipt))
        ipa.write_bytes(ipa.read_bytes() + b'modified')
        with self.assertRaisesRegex(ValueError, 'checksum'):
            prepare_artifact(self.root)

    def test_github_outputs_preserve_paths_with_spaces(self):
        output = self.root / 'github-output'
        write_github_values(output, {'ipa_path': '/workspace with spaces/NiYien.ipa'})
        self.assertEqual(output.read_text(), 'ipa_path=/workspace with spaces/NiYien.ipa\n')
        with self.assertRaises(ValueError):
            write_github_values(output, {'ipa_path': 'one\ntwo'})

    def test_account_upload_reuses_password_without_putting_it_in_arguments(self):
        _, ipa, _ = self.make_ipa()
        environment = self.shared_config()
        def upload(command, **options):
            self.assertIn(str(ipa.resolve()), command)
            self.assertIn('@env:MACOS_ACCOUNT_PASS', command)
            self.assertNotIn(environment['MACOS_ACCOUNT_PASS'], command)
            self.assertEqual(options['env']['MACOS_ACCOUNT_PASS'], environment['MACOS_ACCOUNT_PASS'])
            return subprocess.CompletedProcess(command, 0)
        upload_account(environment, self.root, upload)

    def test_failed_upload_does_not_expose_credentials(self):
        self.make_ipa()
        environment = self.shared_config()
        with self.assertRaisesRegex(ValueError, 'exit code 1') as caught:
            upload_account(environment, self.root, lambda *args, **kwargs: subprocess.CompletedProcess([], 1))
        self.assertNotIn(environment['MACOS_ACCOUNT_PASS'], str(caught.exception))


if __name__ == '__main__':
    unittest.main()
