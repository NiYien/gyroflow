# SPDX-License-Identifier: GPL-3.0-or-later
import hashlib
import io
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import install_mdk_sdk as deps


class MDKSnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.contents = b'compatible MDK and BRAW 6 reader'
        self.relative = 'lib/mdk.framework/Versions/A/mdk'
        self.archive = self.root / 'snapshot.tar.xz'
        with tarfile.open(self.archive, 'w:xz') as bundle:
            entry = tarfile.TarInfo('mdk-sdk/' + self.relative)
            entry.size = len(self.contents)
            bundle.addfile(entry, io.BytesIO(self.contents))
        self.package = {
            'archive': self.archive.name,
            'sha256': deps.sha256(self.archive),
            'runtime_sha256': {self.relative: hashlib.sha256(self.contents).hexdigest()},
        }
        data = {'revision': 'test-pin', 'platforms': {'macos': self.package}}
        pin = patch.object(deps, 'manifest', return_value=data)
        pin.start()
        self.addCleanup(pin.stop)
        self.downloader = Mock(side_effect=lambda url, destination: shutil.copyfile(self.archive, destination))

    def install(self, environment=None):
        return deps.install('macos', self.root, environment or {}, self.downloader)

    def test_verified_cold_install_and_warm_reuse(self):
        sdk = self.install({'SDK_BASE': 'https://mirror.example/sdk/'})
        self.assertEqual((sdk / self.relative).read_bytes(), self.contents)
        self.assertEqual(self.downloader.call_args.args[0], 'https://mirror.example/sdk/snapshot.tar.xz')
        self.downloader.reset_mock()
        self.assertEqual(self.install(), sdk)
        self.downloader.assert_not_called()

    def test_stale_cached_runtime_is_preserved_and_replaced(self):
        sdk = self.install()
        (sdk / self.relative).write_bytes(b'old API')
        self.downloader.reset_mock()
        self.install()
        self.downloader.assert_called_once()
        self.assertEqual((sdk / self.relative).read_bytes(), self.contents)
        backups = list(sdk.parent.glob('mdk-sdk-previous-*'))
        self.assertEqual(len(backups), 1)
        self.assertEqual((backups[0] / self.relative).read_bytes(), b'old API')

    def test_explicit_old_sdk_is_rejected_without_mutation_or_download(self):
        sdk = self.install()
        (sdk / self.relative).write_bytes(b'old API')
        self.downloader.reset_mock()
        with self.assertRaisesRegex(ValueError, 'Unset MDK_SDK'):
            self.install({'MDK_SDK': str(sdk)})
        self.downloader.assert_not_called()
        self.assertEqual((sdk / self.relative).read_bytes(), b'old API')

    def test_corrupt_archive_never_becomes_the_active_sdk(self):
        self.package['sha256'] = '0' * 64
        with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
            self.install()
        self.assertFalse((self.root / 'ext/mdk-test-pin/macos/mdk-sdk').exists())

    def test_archive_link_cannot_escape_extraction_directory(self):
        archive = self.root / 'bad.tar'
        with tarfile.open(archive, 'w') as bundle:
            entry = tarfile.TarInfo('mdk-sdk/escape')
            entry.type = tarfile.SYMTYPE
            entry.linkname = '../../outside'
            bundle.addfile(entry)
        with self.assertRaisesRegex(ValueError, 'Unsafe MDK archive link'):
            deps.extract(archive, self.root / 'extracted')


if __name__ == '__main__':
    unittest.main()
