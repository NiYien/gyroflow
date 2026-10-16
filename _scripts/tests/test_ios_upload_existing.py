# SPDX-License-Identifier: GPL-3.0-or-later
import hashlib
import json
from pathlib import Path
import plistlib
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import ios_upload_existing as upload
from ios_ci import check_config
from package_ios import sign_app


class ExistingIPAUploadTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / '_deployment/ios').mkdir(parents=True)
        (self.root / '_deployment/mobile-version.json').write_text('{"version":"9.0.0"}')
        (self.root / '_deployment/ios/app.json').write_text(json.dumps({
            'display_name': 'NiYien', 'bundle_identifier': 'com.niyien.stabilizer',
            'build_number': '4', 'minimum_os_version': '15.0'}))
        self.source = self.root / 'source'
        (self.source / 'ios').mkdir(parents=True)
        self.commit = 'a' * 40
        self.ipa = self.source / 'NiYien-original.ipa'
        self.info = {'CFBundleIdentifier': 'com.niyien.stabilizer', 'CFBundleShortVersionString': '1.0.2',
                     'CFBundleVersion': '154.1', 'CFBundleExecutable': 'gyroflow', 'DTXcode': '2600',
                     'DTPlatformVersion': '26.0'}
        self.make_ipa()
        self.receipt = {'bundle_identifier': 'com.niyien.stabilizer', 'version': '1.0.2',
                        'build_number': '154.1', 'signing': 'unsigned', 'commit': self.commit,
                        'ipa': '/old/runner/workspace/NiYien-original.ipa'}
        self.save_receipt()

    def make_ipa(self):
        with zipfile.ZipFile(self.ipa, 'w') as archive:
            archive.writestr('Payload/NiYien.app/Info.plist', plistlib.dumps(self.info))
            executable = zipfile.ZipInfo('Payload/NiYien.app/gyroflow')
            executable.create_system = 3
            executable.external_attr = (stat.S_IFREG | 0o755) << 16
            archive.writestr(executable, b'fixture executable')

    def save_receipt(self):
        self.receipt['sha256'] = hashlib.sha256(self.ipa.read_bytes()).hexdigest()
        (self.source / 'ios/build-receipt.json').write_text(json.dumps(self.receipt))

    def test_old_runner_paths_and_old_versions_are_supported(self):
        ipa, receipt = upload.read_source(self.source, self.commit, self.root)
        self.assertEqual(ipa, self.ipa)
        self.assertEqual((receipt['version'], receipt['build_number']), ('1.0.2', '154.1'))

    def test_modified_ipa_and_mismatched_source_commit_are_rejected(self):
        with self.assertRaisesRegex(ValueError, 'commit'):
            upload.read_source(self.source, 'b' * 40, self.root)
        self.ipa.write_bytes(self.ipa.read_bytes() + b'tampered')
        with self.assertRaisesRegex(ValueError, 'checksum'):
            upload.read_source(self.source, self.commit, self.root)

    def test_receipt_must_match_ipa_identity_and_known_app(self):
        for field, value in [('bundle_identifier', 'com.other.app'), ('version', '1.0.3'),
                             ('build_number', '155.1'), ('signing', 'other')]:
            previous = self.receipt[field]
            self.receipt[field] = value
            self.save_receipt()
            with self.subTest(field=field), self.assertRaises(ValueError):
                upload.read_source(self.source, self.commit, self.root)
            self.receipt[field] = previous

    def test_ambiguous_or_missing_ipa_fails(self):
        extra = self.source / 'other.ipa'
        extra.write_bytes(self.ipa.read_bytes())
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            upload.read_source(self.source, self.commit, self.root)
        extra.unlink()
        self.ipa.unlink()
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            upload.read_source(self.source, self.commit, self.root)

    def test_unsafe_entries_are_rejected_before_extracting_anything(self):
        entries = ['../escape', '/absolute', 'Payload/NiYien.app/../../escape',
                   'Payload/NiYien.app\\escape', 'Payload/NiYien.app/info.plist',
                   'Payload/Other.app/Info.plist', 'Payload/NiYien.app/PlugIns/Test.appex/Info.plist']
        for index, name in enumerate(entries):
            with self.subTest(name=name):
                self.make_ipa()
                with zipfile.ZipFile(self.ipa, 'a') as archive:
                    member = zipfile.ZipInfo(name)
                    member.filename = name
                    archive.writestr(member, b'invalid')
                stage = self.root / f'extract-{index}'
                with self.assertRaises(ValueError):
                    upload.extract_ipa(self.ipa, stage)
                self.assertFalse(stage.exists())

    def test_symlinks_in_ipa_are_rejected(self):
        with zipfile.ZipFile(self.ipa, 'a') as archive:
            link = zipfile.ZipInfo('Payload/NiYien.app/link')
            link.create_system = 3
            link.external_attr = (stat.S_IFLNK | 0o777) << 16
            archive.writestr(link, '/outside')
        with self.assertRaisesRegex(ValueError, 'unsafe'):
            upload.extract_ipa(self.ipa, self.root / 'extract')

    def test_original_sdk_and_device_platform_are_checked(self):
        for platform in ['platform IOS\nsdk 18.0', 'platform IOSSIMULATOR\nsdk 26.0']:
            with self.subTest(platform=platform), patch.object(upload, 'output', return_value=platform):
                with tempfile.TemporaryDirectory(dir=self.root) as stage, self.assertRaises(ValueError):
                    upload.extract_ipa(self.ipa, Path(stage))

    @patch.object(upload, 'output', return_value='platform IOS\nsdk 26.0')
    def test_unsigned_inspection_requests_signing_without_changing_version(self, _):
        result = upload.inspect_source(self.source, self.commit, self.root)
        self.assertEqual(result, {'needs_signing': 'true', 'version': '1.0.2', 'build_number': '154.1'})

    @patch.object(upload, 'output', return_value='platform IOS\nsdk 26.0')
    @patch.object(upload, 'verify_distribution')
    def test_existing_distribution_signature_is_verified_and_bytes_preserved(self, verify, _):
        self.receipt['signing'] = 'distribution'
        self.save_receipt()
        with patch.object(upload, 'sign_app') as sign, patch.object(upload, 'signing_settings') as settings:
            result = upload.prepare_upload(self.source, self.commit, self.root)
        self.assertEqual(Path(result['ipa_path']).read_bytes(), self.ipa.read_bytes())
        verify.assert_called_once()
        sign.assert_not_called()
        settings.assert_not_called()

    @patch.object(upload, 'output', return_value='platform IOS\nsdk 26.0')
    @patch.object(upload, 'verify_distribution', side_effect=ValueError('invalid signature'))
    def test_distribution_receipt_cannot_bypass_signature_validation(self, *_):
        self.receipt['signing'] = 'distribution'
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, 'invalid signature'):
            upload.inspect_source(self.source, self.commit, self.root)

    @patch.object(upload, 'output', return_value='platform IOS\nsdk 26.0')
    @patch.object(upload, 'verify_distribution')
    @patch.object(upload, 'signing_settings', return_value=('profile', 'key', {}))
    def assert_resigning_preserves_payload(self, settings, verify, _):
        def sign(app, stage, signing):
            (app / 'embedded.mobileprovision').write_bytes(b'fixture distribution profile')
        def zip_ipa(*args, **kwargs):
            self.assertEqual(args[:2], ('zip', '-qr'))
            stage = kwargs['cwd']
            with zipfile.ZipFile(args[2], 'w') as archive:
                for file in (stage / 'Payload').rglob('*'):
                    if file.is_file():
                        archive.write(file, file.relative_to(stage).as_posix())
        with patch.object(upload, 'sign_app', side_effect=sign), patch.object(upload, 'run', side_effect=zip_ipa):
            result = upload.prepare_upload(self.source, self.commit, self.root)
        target = Path(result['ipa_path'])
        with zipfile.ZipFile(target) as archive:
            self.assertEqual(plistlib.loads(archive.read('Payload/NiYien.app/Info.plist')), self.info)
            self.assertEqual(archive.read('Payload/NiYien.app/gyroflow'), b'fixture executable')
        receipt = json.loads((target.parent / 'ios/build-receipt.json').read_text())
        self.assertEqual(receipt['signing'], 'distribution')
        self.assertEqual(receipt['source_sha256'], self.receipt['sha256'])
        self.assertNotEqual(receipt['sha256'], receipt['source_sha256'])
        self.assertEqual(receipt['sha256'], hashlib.sha256(target.read_bytes()).hexdigest())
        settings.assert_called_once_with('distribution', self.receipt)
        verify.assert_called_once()

    def test_unsigned_ipa_is_signed_without_rebuilding_or_changing_version(self):
        self.assert_resigning_preserves_payload()

    def test_development_ipa_is_resigned_for_distribution(self):
        self.receipt['signing'] = 'development'
        self.save_receipt()
        self.assert_resigning_preserves_payload()

    def test_signed_upload_only_requires_upload_credentials(self):
        environment = {'IOS_UPLOAD_TO_CONNECT': 'true', 'MACOS_ACCOUNT_USER': 'fixture', 'MACOS_ACCOUNT_PASS': 'secret'}
        self.assertEqual(check_config(environment, self.root, require_signing=False)['upload_mode'], 'account')
        with self.assertRaisesRegex(ValueError, 'IOS_CERTIFICATES'):
            check_config(environment, self.root)

    def test_shared_signer_signs_frameworks_before_the_app_and_verifies(self):
        app = self.root / 'NiYien.app'
        framework = app / 'Frameworks/mdk.framework'
        framework.mkdir(parents=True)
        profile = self.root / 'profile'
        profile.write_bytes(b'profile')
        granted = {'application-identifier': 'TEAM.com.niyien.stabilizer', 'get-task-allow': False}
        with patch('package_ios.run') as command:
            sign_app(app, self.root, (profile, 'A' * 40, granted))
        calls = command.call_args_list
        self.assertEqual(calls[0].args[-1], framework)
        self.assertEqual(calls[1].args[-1], app)
        self.assertEqual(calls[2].args[:4], ('codesign', '--verify', '--deep', '--strict'))
        entitlements = plistlib.loads((self.root / 'entitlements.xcent').read_bytes())
        self.assertEqual(entitlements['keychain-access-groups'], ['TEAM.com.niyien.stabilizer'])

    def test_distribution_verifier_checks_embedded_profile_certificate_and_entitlements(self):
        app = self.root / 'NiYien.app'
        app.mkdir()
        certificate = b'fixture signer certificate'
        (self.root / 'signer-0').write_bytes(certificate)
        granted = {'application-identifier': 'TEAM.com.niyien.stabilizer',
                   'com.apple.developer.team-identifier': 'TEAM', 'get-task-allow': False}
        with patch.object(upload, 'run'), patch.object(upload, 'signing_settings', return_value=(None, None, granted)) as settings:
            with patch.object(upload.subprocess, 'check_output', return_value=plistlib.dumps(granted)):
                upload.verify_distribution(app, self.receipt, self.root)
            settings.assert_called_once_with('distribution', self.receipt, profile=app / 'embedded.mobileprovision',
                                             key=hashlib.sha1(certificate).hexdigest())
            for actual in [{**granted, 'get-task-allow': True}, {**granted, 'application-identifier': 'OTHER.app'}]:
                with patch.object(upload.subprocess, 'check_output', return_value=plistlib.dumps(actual)):
                    with self.assertRaises(ValueError):
                        upload.verify_distribution(app, self.receipt, self.root)


if __name__ == '__main__':
    unittest.main()
