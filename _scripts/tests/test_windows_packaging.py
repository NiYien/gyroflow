# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise SDK extraction and Windows packaging failure paths without a full build."""
import hashlib
import importlib.util
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('r3d_builder', ROOT / '_scripts/build_r3d_plugin.py')
BUILDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILDER)
SEVEN_ZIP = shutil.which('7z')
POWERSHELL = shutil.which('powershell.exe')


@unittest.skipUnless(sys.platform == 'win32' and SEVEN_ZIP, 'requires Windows and 7-Zip')
class RedSdkExtractionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='windows packaging ')
        self.addCleanup(self.temporary.cleanup)
        self.work = Path(self.temporary.name)
        self.cache = self.work / 'cache with spaces'
        self.cache.mkdir()
        self.archive = self.cache / 'sdk.7z'
        self.sentinel = 'R3DSDK/Include/R3DSDK.h'

    def install(self):
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        packages = ((self.archive.name, 'https://unused.invalid/sdk.7z', digest, self.sentinel),)
        with mock.patch.object(BUILDER, 'CACHE', self.cache), \
                mock.patch.object(BUILDER, 'PACKAGES', packages), \
                mock.patch.object(BUILDER.urllib.request, 'urlopen', side_effect=AssertionError('unexpected download')):
            BUILDER.install_inputs()
        return digest

    def test_lzma_archive_extracts_and_verified_cache_is_reused(self):
        source = self.work / 'source'
        header = source / self.sentinel
        header.parent.mkdir(parents=True)
        header.write_bytes(b'RED SDK test header\n')
        subprocess.run([SEVEN_ZIP, 'a', '-t7z', '-m0=LZMA', str(self.archive), 'R3DSDK'],
                       cwd=source, check=True, capture_output=True)
        digest = self.install()
        self.assertEqual((self.cache / self.sentinel).read_bytes(), header.read_bytes())
        self.assertEqual((self.cache / 'sdk.7z.verified').read_text(), digest)
        with mock.patch.object(BUILDER.subprocess, 'run', side_effect=AssertionError('unexpected extraction')):
            self.install()

    def test_failed_extraction_does_not_mark_cache_verified(self):
        self.archive.write_bytes(b'not a 7z archive')
        with self.assertRaises(subprocess.CalledProcessError):
            self.install()
        self.assertFalse((self.cache / 'sdk.7z.verified').exists())

    def test_missing_7zip_reports_actionable_error(self):
        self.archive.write_bytes(b'unused archive')
        with mock.patch.object(BUILDER.shutil, 'which', return_value=None):
            with self.assertRaisesRegex(RuntimeError, '7-Zip .* required on PATH'):
                self.install()
        self.assertFalse((self.cache / 'sdk.7z.verified').exists())


@unittest.skipUnless(sys.platform == 'win32' and POWERSHELL, 'requires Windows PowerShell')
class WindowsPackageGuardTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.recipe = (ROOT / '_scripts/windows.just').read_text(encoding='utf-8')

    def run_script(self, body, work):
        script = work / 'verify.ps1'
        script.write_text("$ErrorActionPreference = 'Stop'\n" + body, encoding='utf-8')
        return subprocess.run([POWERSHELL, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(script)],
                              cwd=work, capture_output=True, text=True)

    def test_cargo_failure_stops_before_packaging(self):
        start = self.recipe.index('    cargo build --profile deploy --target $Target')
        end = self.recipe.index('    $TargetDir', start)
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            result = self.run_script(
                'function cargo { cmd.exe /c "exit /b 101" }\n' + self.recipe[start:end] +
                "Set-Content -LiteralPath reached-packaging -Value unexpected\n", work)
            self.assertEqual(result.returncode, 101, result.stderr)
            self.assertFalse((work / 'reached-packaging').exists())

    def test_missing_build_exe_cannot_reuse_stale_staged_exe(self):
        copy = next(line for line in self.recipe.splitlines()
                    if 'Copy-Item -LiteralPath "$CargoTarget\\gyroflow.exe"' in line)
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'staging').mkdir()
            (work / 'staging/Gyroflow.exe').write_bytes(b'stale build')
            result = self.run_script('$CargoTarget = "missing-build"\n$TargetDir = "staging"\n' + copy +
                                     '\nSet-Content reached-packaging unexpected\n', work)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((work / 'reached-packaging').exists())

    def test_zip_failure_stops_before_setup(self):
        start = self.recipe.index('    7z a $PackagePath $TargetDir\\*')
        end = self.recipe.index('    if ("{{Arch}}"', start)
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            result = self.run_script(
                'function 7z { cmd.exe /c "exit /b 2" }\n' + self.recipe[start:end] +
                '\nSet-Content reached-setup unexpected\n', work)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Failed to create Windows package', result.stderr)
            self.assertFalse((work / 'reached-setup').exists())

    def test_pre_archive_guard_rejects_missing_or_empty_exe(self):
        start = self.recipe.index('    if (-not (Test-Path -LiteralPath "$TargetDir\\Gyroflow.exe"')
        end = self.recipe.index('    $PackageName', start)
        for contents in (None, b'', b'MZ test executable'):
            with self.subTest(contents=contents), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                if contents is not None:
                    (work / 'Gyroflow.exe').write_bytes(contents)
                result = self.run_script('$TargetDir = $PWD\n' + self.recipe[start:end] +
                                         '\nSet-Content reached-packaging allowed\n', work)
                self.assertEqual(result.returncode == 0, bool(contents), result.stderr)
                self.assertEqual((work / 'reached-packaging').exists(), bool(contents))


if __name__ == '__main__':
    unittest.main()
