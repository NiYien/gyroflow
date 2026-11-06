# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise the shared HEVC compatibility policy without Qt or the MDK runtime."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class HevcParameterSetWorkaroundTests(unittest.TestCase):
    def test_only_complete_single_layer_multi_pps_records_are_eligible(self):
        compiler = shutil.which('clang++') or shutil.which('g++')
        if not compiler:
            self.skipTest('requires a C++17 compiler')
        with tempfile.TemporaryDirectory() as work:
            executable = Path(work) / 'hevc_parameter_set_workaround'
            subprocess.run([compiler, '-std=c++17', '-Wall', '-Wextra', '-Werror',
                            str(ROOT / '_scripts/tests/hevc_parameter_set_workaround.cpp'),
                            '-o', str(executable)], check=True, capture_output=True, text=True)
            result = subprocess.run([str(executable)], check=True, capture_output=True, text=True)
            self.assertIn('PASS:', result.stdout)


if __name__ == '__main__':
    unittest.main()
