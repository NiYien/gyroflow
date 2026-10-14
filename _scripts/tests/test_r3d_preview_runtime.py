# SPDX-License-Identifier: GPL-3.0-or-later
"""Opt-in Metal R3D regression test; set NIYIEN_R3D_TEST_CLIP to a >=3s clip."""
import os
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CLIP = os.environ.get('NIYIEN_R3D_TEST_CLIP')


@unittest.skipUnless(platform.system() == 'Darwin' and platform.machine() == 'arm64' and CLIP,
                     'requires Apple Silicon and NIYIEN_R3D_TEST_CLIP')
class R3DPreviewRuntimeTests(unittest.TestCase):
    def test_pause_scrub_and_resume_do_not_replay_stale_frames(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('mdk_installer', ROOT / '_scripts/install_mdk_sdk.py')
        installer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(installer)
        sdk = Path(installer.install('macos'))
        frameworks = ROOT / 'target/Frameworks'
        self.assertTrue((frameworks / 'mdk.framework').exists(), 'Run just run before the runtime test')
        with tempfile.TemporaryDirectory(dir=ROOT / 'target') as work:
            work = Path(work)
            (work / 'Frameworks').symlink_to(frameworks, target_is_directory=True)
            executable = work / 'r3d_preview_lifecycle'
            subprocess.run(['xcrun', 'clang++', '-std=c++17', '-fobjc-arc',
                            str(ROOT / '_scripts/tests/r3d_preview_lifecycle.mm'),
                            '-I', str(sdk / 'include'), '-F', str(frameworks), '-framework', 'mdk',
                            '-framework', 'Metal', '-framework', 'Foundation',
                            '-Wl,-rpath,@executable_path/Frameworks', '-o', str(executable)], check=True)
            decoder = 'R3D:gpu=metal:decompress=async:format=rgb48le:copy=1:scale=1920x1080'
            result = subprocess.run([str(executable), CLIP, decoder, '2'], capture_output=True,
                                    text=True, timeout=30)
            if result.returncode == 77:
                self.skipTest('The sample must be at least three seconds long')
            self.assertEqual(result.returncode, 0, result.stderr[-4000:])
            self.assertIn('Using audio backend: AudioQueue', result.stderr)
            for error in ('Audio output rejected frame', 'Invalid Operation',
                          'ERROR: AudioQueueEnqueueBuffer', 'AudioQueueStart ERROR'):
                self.assertNotIn(error, result.stderr, result.stderr[-4000:])
            frames = {i: [] for i in range(1, 9)}
            starts = {}
            for line in result.stdout.splitlines():
                fields = line.split()
                if fields and fields[0] == 'PLAY':
                    starts[int(fields[1])] = float(fields[2])
                if fields and fields[0] == 'RENDER' and int(fields[1]) in frames:
                    frames[int(fields[1])].append((float(fields[2]), float(fields[3])))
            first_motion = next(t for t, pts in frames[1] if pts > 0)
            self.assertLess(first_motion - starts[1], 75, frames[1])
            for phase in (1, 2, 3, 6, 7):
                samples = frames[phase]
                self.assertGreaterEqual(len(samples), 12, (phase, samples))
                self.assertGreater(samples[-1][1] - samples[0][1], .15, (phase, samples))
                self.assertTrue(all(b[1] >= a[1] for a, b in zip(samples, samples[1:])), (phase, samples))
                gaps = [b[0] - a[0] for a, b in zip(samples, samples[1:])]
                self.assertLess(statistics.median(gaps), 40, (phase, gaps))
                self.assertLess(max(gaps), 75 if phase == 1 else 180, (phase, gaps))
            # Once the new seek target is visible, the old decode chain must not return.
            forward = frames[4]
            first = next(i for i, (_, pts) in enumerate(forward) if pts >= 2.1)
            self.assertTrue(all(pts >= 2.1 for _, pts in forward[first:]), forward)
            backward = frames[5]
            first = next(i for i, (_, pts) in enumerate(backward) if .39 <= pts <= .5)
            self.assertTrue(all(.39 <= pts < 1.2 for _, pts in backward[first:]), backward)
            self.assertTrue(all(b[1] >= a[1] for a, b in zip(backward[first:], backward[first+1:])), backward)
            loop = frames[8]
            self.assertGreaterEqual(len(loop), 20, loop)
            self.assertTrue(any(b[1] < a[1] for a, b in zip(loop, loop[1:])), loop)
            self.assertLess(loop[-1][1], 1.0, loop)

    def test_preview_audio_preserves_every_source_sample(self):
        frameworks = ROOT / 'target/Frameworks'
        abi = ROOT / 'ext/r3d-preview/include/abi'
        self.assertTrue((abi / 'mdk/FrameReader.h').exists(), 'Run just run before the runtime test')
        with tempfile.TemporaryDirectory(dir=ROOT / 'target') as work:
            work = Path(work)
            (work / 'Frameworks').symlink_to(frameworks, target_is_directory=True)
            executable = work / 'r3d_audio_samples'
            subprocess.run(['xcrun', 'clang++', '-std=c++2b', '-DBUILD_MDK_LIB', '-DMDK_ABI=abi',
                            str(ROOT / '_scripts/tests/r3d_audio_samples.cpp'),
                            '-I', str(abi), '-F', str(frameworks), '-framework', 'mdk',
                            '-Wl,-rpath,@executable_path/Frameworks', '-o', str(executable)], check=True)
            result = subprocess.run([str(executable), CLIP], capture_output=True, text=True, timeout=40)
            self.assertEqual(result.returncode, 0, result.stderr[-4000:])
            self.assertIn('PCM_IDENTICAL ', result.stdout)


if __name__ == '__main__':
    unittest.main()
