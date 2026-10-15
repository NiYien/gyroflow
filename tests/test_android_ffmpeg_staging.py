# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise the production Android staging step with an old incremental APK tree."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
POWERSHELL = shutil.which('powershell.exe')
LIBRARIES = ('avcodec', 'avdevice', 'avfilter', 'avformat', 'avutil', 'swresample', 'swscale')


@unittest.skipUnless(POWERSHELL, 'Android packaging uses Windows PowerShell')
class AndroidFfmpegStagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.sdk = self.root / 'sdk/lib'
        self.stage = self.root / 'target/android-build/libs/arm64-v8a'
        self.sdk.mkdir(parents=True)
        self.stage.mkdir(parents=True)
        for name in LIBRARIES:
            (self.sdk / f'lib{name}.so').write_bytes(b'new-sdk-' + name.encode())
            (self.stage / f'lib{name}.so').write_bytes(b'old-cargo-apk-library')
        (self.stage / 'libmdk.so').write_bytes(b'pinned-player')
        source = (ROOT / '_scripts/android.just').read_text(encoding='utf-8')
        start = source.index('    foreach ($Library in @("avcodec"')
        end = source.index('\n        }', start) + len('\n        }')
        self.script = self.root / 'stage.ps1'
        self.script.write_text(
            "$ErrorActionPreference = 'Stop'\n$ProjectDir = $PSScriptRoot\n"
            "$Env:FFMPEG_DIR = Join-Path $PSScriptRoot 'sdk'\n" + source[start:end],
            encoding='utf-8',
        )

    def run_stage(self):
        return subprocess.run([POWERSHELL, '-NoProfile', '-ExecutionPolicy', 'Bypass',
                               '-File', str(self.script)], capture_output=True)

    def test_every_old_split_library_is_replaced_without_changing_the_player(self):
        result = self.run_stage()
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
        for name in LIBRARIES:
            self.assertEqual((self.stage / f'lib{name}.so').read_bytes(),
                             (self.sdk / f'lib{name}.so').read_bytes())
        self.assertEqual((self.stage / 'libmdk.so').read_bytes(), b'pinned-player')

    def test_missing_sdk_library_stops_packaging_instead_of_reusing_the_old_one(self):
        (self.sdk / 'libavcodec.so').unlink()
        self.assertNotEqual(self.run_stage().returncode, 0)


if __name__ == '__main__':
    unittest.main()
