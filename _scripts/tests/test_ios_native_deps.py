# SPDX-License-Identifier: GPL-3.0-or-later
import hashlib
import io
import os
from pathlib import Path
import plistlib
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import ios_native_deps as deps
import package_ios


class IOSNativeDependenciesTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.sdk_files = {
            'mdk-sdk/lib/mdk.xcframework/ios-arm64/mdk.framework/mdk': b'device mdk',
            'mdk-sdk/lib/mdk.xcframework/ios-arm64/mdk.framework/Info.plist':
                plistlib.dumps({'CFBundleVersion': '0.38.0'}),
            'mdk-sdk/lib/mdk.xcframework/ios-arm64/mdk.framework/Headers/Player.h': b'public header',
            'mdk-sdk/lib/mdk.xcframework/ios-arm64/mdk.framework/Headers/c/Player.h': b'C header',
            'mdk-sdk/lib/mdk.xcframework/ios-arm64/mdk.framework/_CodeSignature/CodeResources': b'old signature',
            'mdk-sdk/lib/mdk.xcframework/ios-arm64_x86_64-simulator/mdk.framework/mdk': b'simulator mdk',
        }
        self.braw_files = {}
        for name in deps.BRAW_FRAMEWORKS:
            self.braw_files[f'{name}.framework/{name}'] = name.encode()
            self.braw_files[f'{name}.framework/Info.plist'] = plistlib.dumps({'CFBundleVersion': '5.0'})
        self.mdk_archive = self.archive(self.sdk_files)
        self.braw_archive = self.archive(self.braw_files)
        self.downloader = Mock(side_effect=self.download)
        for name, value in [('MDK_SHA256', self.mdk_archive), ('BRAW_SHA256', self.braw_archive)]:
            patcher = patch.object(deps, name, hashlib.sha256(value).hexdigest())
            patcher.start()
            self.addCleanup(patcher.stop)

    def archive(self, files):
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode='w:gz') as archive:
            for name, contents in files.items():
                member = tarfile.TarInfo(name)
                member.size = len(contents)
                member.mtime = 1700000000
                archive.addfile(member, io.BytesIO(contents))
        return buffer.getvalue()

    def download(self, url, destination):
        destination.write_bytes(self.mdk_archive if url == deps.MDK_URL else self.braw_archive)

    def install(self, environment=None):
        return deps.install(self.root, environment or {}, self.downloader)

    def test_cold_install_supplies_headers_legacy_probe_and_device_frameworks(self):
        sdk = self.install()
        self.assertEqual(self.downloader.call_count, 2)
        self.assertEqual((sdk / 'include/mdk/Player.h').read_bytes(), b'public header')
        self.assertEqual((sdk / 'include/mdk/c/Player.h').read_bytes(), b'C header')
        self.assertEqual((sdk / 'lib/mdk.framework/mdk').read_bytes(), b'device mdk')
        frameworks = deps.runtime_frameworks(self.root, {})
        self.assertEqual([p.name for p in frameworks],
                         ['mdk.framework', 'BlackmagicRawAPI.framework', 'DecoderMetal.framework'])
        self.assertIn('ios-arm64', frameworks[0].parts)

    def test_warm_install_does_not_download_or_replace_local_sdk(self):
        sdk = self.install()
        binary = deps.mdk_framework(sdk) / 'mdk'
        binary.write_bytes(b'existing authorized SDK')
        self.downloader.reset_mock()
        self.assertEqual(self.install(), sdk)
        self.downloader.assert_not_called()
        self.assertEqual(binary.read_bytes(), b'existing authorized SDK')

    def test_packaging_works_without_cargo_frameworks_after_cache_cleanup(self):
        self.install()
        stale = self.root / 'target/aarch64-apple-ios/Frameworks/mdk.framework'
        stale.mkdir(parents=True)
        (stale / 'mdk').write_bytes(b'stale build output')
        first = self.root / 'first/NiYien.app/Frameworks'
        deps.copy_runtime_frameworks(first, self.root, {})
        stale.parent.rename(stale.parent.with_name('Frameworks-before-cleanup'))
        second = self.root / 'second/NiYien.app/Frameworks'
        deps.copy_runtime_frameworks(second, self.root, {})
        for name in ('mdk', *deps.BRAW_FRAMEWORKS):
            relative = f'{name}.framework/{name}'
            self.assertEqual((first / relative).read_bytes(), (second / relative).read_bytes())
            self.assertFalse((second / relative).is_symlink())
        self.assertEqual((second / 'mdk.framework/mdk').read_bytes(), b'device mdk')
        self.assertFalse((second / 'mdk.framework/Headers').exists())
        self.assertFalse((second / 'mdk.framework/_CodeSignature').exists())

    def test_explicit_sdk_is_used_by_both_build_and_package(self):
        sdk = self.install()
        custom = (self.root / 'custom SDK').resolve()
        sdk.rename(custom)
        environment = {'MDK_SDK': str(custom)}
        self.downloader.reset_mock()
        self.assertEqual(self.install(environment), custom)
        self.assertEqual(deps.runtime_frameworks(self.root, environment)[0], deps.mdk_framework(custom))
        self.downloader.assert_not_called()

    def test_missing_explicit_sdk_never_falls_back_to_another_download(self):
        with self.assertRaisesRegex(ValueError, 'Explicit MDK_SDK'):
            self.install({'MDK_SDK': str(self.root / 'missing')})
        self.downloader.assert_not_called()

    def test_checksum_failure_never_installs_partial_sdk(self):
        with patch.object(deps, 'MDK_SHA256', '0' * 64):
            with self.assertRaisesRegex(ValueError, 'checksum'):
                self.install()
        self.assertFalse((self.root / 'ext/mdk-sdk-ios').exists())

    def test_unsafe_archive_is_rejected_before_extraction(self):
        self.mdk_archive = self.archive({'../escape': b'invalid'})
        with patch.object(deps, 'MDK_SHA256', hashlib.sha256(self.mdk_archive).hexdigest()):
            with self.assertRaisesRegex(ValueError, 'archive entry'):
                self.install()
        self.assertFalse((self.root / 'ext/mdk-sdk-ios').exists())

    def test_missing_braw_is_detected_before_creating_app_frameworks(self):
        self.install()
        (deps.braw_directory(self.root) / 'DecoderMetal.framework/DecoderMetal').unlink()
        destination = self.root / 'NiYien.app/Frameworks'
        with self.assertRaisesRegex(ValueError, 'DecoderMetal'):
            deps.copy_runtime_frameworks(destination, self.root, {})
        self.assertFalse(destination.exists())

    def test_unsigned_ipa_contains_all_runtime_binaries_without_cargo_staging(self):
        self.install()
        metadata = {'display_name': 'NiYien', 'bundle_identifier': 'com.niyien.stabilizer',
                    'version': '1.0.1', 'build_number': '5.1', 'minimum_os_version': '15.0'}
        executable = self.root / 'target/aarch64-apple-ios/deploy/gyroflow'
        executable.parent.mkdir(parents=True)
        executable.write_bytes(b'application')
        deployment = self.root / '_deployment/ios'
        deployment.mkdir(parents=True)
        (deployment / 'Info.plist').write_bytes(plistlib.dumps({}))
        for name in ('PkgInfo', 'PrivacyInfo.xcprivacy'):
            (deployment / name).write_bytes(b'fixture')

        def output(*arguments):
            return {('xcrun', 'vtool'): 'platform IOS\nsdk 26.0\n',
                    ('xcode-select', '-p'): '/Xcode.app/Contents/Developer',
                    ('git', 'status'): ''}.get(arguments[:2], 'fixture')

        def run(*arguments, **options):
            if arguments[0] == 'zip':
                with zipfile.ZipFile(arguments[2], 'w') as archive:
                    for path in (options['cwd'] / 'Payload').rglob('*'):
                        if path.is_file():
                            archive.write(path, path.relative_to(options['cwd']))

        with patch.object(package_ios, 'ROOT', self.root), patch.object(package_ios, 'load_metadata', return_value=metadata), \
                patch.object(package_ios, 'output', side_effect=output), patch.object(package_ios, 'run', side_effect=run), \
                patch.object(package_ios, 'make_icons', return_value={}), \
                patch.object(package_ios, 'xcode_toolchain_metadata', return_value={}), \
                patch.dict(os.environ, {'MDK_SDK': str(deps.mdk_directory(self.root, {}))}), \
                patch('builtins.print'):
            package_ios.package('deploy', 'unsigned')
        ipa = next((self.root / '_deployment/_binaries').glob('*.ipa'))
        with zipfile.ZipFile(ipa) as archive:
            self.assertEqual(archive.read('Payload/NiYien.app/Frameworks/mdk.framework/mdk'), b'device mdk')
            for name in deps.BRAW_FRAMEWORKS:
                self.assertEqual(archive.read(f'Payload/NiYien.app/Frameworks/{name}.framework/{name}'), name.encode())


if __name__ == '__main__':
    unittest.main()
