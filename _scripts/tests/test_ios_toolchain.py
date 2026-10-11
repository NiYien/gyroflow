# SPDX-License-Identifier: GPL-3.0-or-later
"""Compile the ARM intrinsic using the flags actually exported by ios.just."""
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
QT_INCLUDE = ROOT / 'ext/6.7.3/ios/include'


@unittest.skipUnless(sys.platform == 'darwin' and shutil.which('just') and shutil.which('xcrun')
                     and (QT_INCLUDE / 'QtCore/QByteArray').is_file(), 'Requires the installed iOS Qt toolchain')
class IOSCompilerFlagsTests(unittest.TestCase):
    def compile_probe(self, extra_flags=None):
        environment = os.environ.copy()
        environment.pop('CXXFLAGS_aarch64_apple_ios', None)
        if extra_flags:
            environment['CXXFLAGS_aarch64_apple_ios'] = extra_flags
        flags = subprocess.check_output([
            'just', '--justfile', str(ROOT / '_scripts/ios.just'), '--evaluate', 'CXXFLAGS_aarch64_apple_ios',
        ], cwd=ROOT, env=environment, text=True)
        sdk = subprocess.check_output(['xcrun', '--sdk', 'iphoneos', '--show-sdk-path'], text=True).strip()
        source = '#include <QtCore/QByteArray>\nvoid probe() { qYieldCpu(); __yield(); }\n'
        if extra_flags:
            source += '#ifndef NIYIEN_CXX_PROBE\n#error Caller flags were lost\n#endif\n'
        result = subprocess.run([
            'xcrun', '--sdk', 'iphoneos', 'clang++', '--target=arm64-apple-ios',
            '-miphoneos-version-min=15.0', '-isysroot', sdk, '-I', str(QT_INCLUDE),
            *shlex.split(flags), '-std=c++17', '-fsyntax-only', '-x', 'c++', '-',
        ], input=source, env=environment, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_qt_and_arm_yield_compile_without_private_local_wrappers(self):
        self.compile_probe()

    def test_caller_target_flags_are_preserved(self):
        self.compile_probe('-DNIYIEN_CXX_PROBE=1')


if __name__ == '__main__':
    unittest.main()
