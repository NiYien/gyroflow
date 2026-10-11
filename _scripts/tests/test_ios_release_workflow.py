# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise the iOS matrix routing and build commands in the release workflow."""
import fnmatch
import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
from types import SimpleNamespace
import unittest

ROOT = Path(__file__).resolve().parents[2]


class IOSReleaseWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = (ROOT / '.github/workflows/release.yml').read_text(encoding='utf-8')
        cls.build = cls.workflow.split('\n  build:', 1)[1].split('\n  github_release:', 1)[0]
        cls.steps = {}
        for block in re.split(r'(?m)^      - ', cls.build)[1:]:
            first, _, rest = block.partition('\n')
            name = first.split(': ', 1)[1]
            rest = re.split(r'(?m)^      #', rest, maxsplit=1)[0]
            cls.steps[name] = first + '\n' + textwrap.dedent(rest)
        cls.platforms = dict((kind, runner) for runner, kind in re.findall(
            r'\{ os: ([\w.-]+),\s+type: ([\w-]+) \}', cls.build))

    def field(self, block, name):
        match = re.search(r'^' + re.escape(name) + r': (.+)$', block, re.MULTILINE)
        self.assertIsNotNone(match, (name, block))
        return match.group(1)

    def expression(self, value, platform, enabled='false', mode='none', profile='none', run=1):
        value = value.removeprefix('${{').removesuffix('}}').strip()
        context = {
            'matrix': SimpleNamespace(targets=SimpleNamespace(type=platform, os=self.platforms[platform])),
            'steps': SimpleNamespace(config=SimpleNamespace(outputs=SimpleNamespace(
                upload_enabled=enabled, upload_mode=mode, profile_mode=profile))),
            'github': SimpleNamespace(run_id=run),
            'format': lambda template, *args: template.format(*args),
        }
        return eval(value.replace('&&', ' and ').replace('||', ' or '), {'__builtins__': {}}, context)

    def enabled(self, step, platform, **options):
        return bool(self.expression(self.field(self.steps[step], 'if'), platform, **options))

    def test_one_release_run_contains_all_five_platforms(self):
        self.assertEqual(set(self.platforms), {'windows', 'macos', 'linux', 'android', 'ios'})
        self.assertEqual(self.platforms['ios'], 'macos-26')
        self.assertFalse((ROOT / '.github/workflows/ios-release.yml').exists())
        options = self.workflow.split('      upload_to_connect:', 1)[1].split('\n\n', 1)[0]
        self.assertIn('default: false', options)
        self.assertIn('type: boolean', options)

    def test_ios_build_verification_and_artifact_steps_only_run_on_ios(self):
        steps = ['Test iOS build helpers', 'Check iOS configuration', 'Build IPA',
                 'Verify IPA metadata', 'Save IPA and debug symbols']
        for platform in self.platforms:
            for step in steps:
                with self.subTest(platform=platform, step=step):
                    self.assertEqual(self.enabled(step, platform), platform == 'ios')

    def test_signing_and_upload_steps_require_both_ios_and_checked_option(self):
        signing = ['Install Apple Distribution certificate', 'Select matching provisioning profile']
        for platform in self.platforms:
            for enabled in ('false', 'true'):
                for mode in ('account', 'api'):
                    for profile in ('embedded', 'api'):
                        with self.subTest(platform=platform, enabled=enabled, mode=mode, profile=profile):
                            options = dict(enabled=enabled, mode=mode, profile=profile)
                            requested = platform == 'ios' and enabled == 'true'
                            for step in signing:
                                self.assertEqual(self.enabled(step, platform, **options), requested)
                            self.assertEqual(self.enabled('Download App Store provisioning profiles', platform, **options),
                                             requested and profile == 'api')
                            self.assertEqual(self.enabled('Upload using existing Apple account', platform, **options),
                                             requested and mode == 'account')
                            self.assertEqual(self.enabled('Upload using App Store Connect API', platform, **options),
                                             requested and mode == 'api')

    @unittest.skipIf(os.name == 'nt', 'The iOS build step executes in bash on macOS')
    def test_actual_build_shell_selects_unsigned_or_distribution_recipe(self):
        script = textwrap.dedent(self.steps['Build IPA'].split('run: |\n', 1)[1])
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            runner = directory / 'just'
            runner.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$IOS_TEST_CALLS"\n')
            runner.chmod(0o755)
            for enabled, recipe in [('false', 'ios build-and-package deploy unsigned'), ('true', 'ios build-ipa')]:
                calls = directory / (enabled + '.log')
                environment = {**os.environ, 'PATH': str(directory) + os.pathsep + os.environ['PATH'],
                               'IOS_UPLOAD_ENABLED': enabled, 'IOS_TEST_CALLS': str(calls)}
                subprocess.run(['bash', '-e', '-c', script], env=environment, check=True)
                self.assertEqual(calls.read_text().splitlines(), ['ios install-deps', recipe])

    def test_ios_keeps_tag_annotations_and_does_not_use_desktop_cache_cleanup(self):
        checkout = self.steps['actions/checkout@v4']
        depth = re.search(r'fetch-depth: (.+)', checkout).group(1)
        for platform in self.platforms:
            self.assertEqual(str(self.expression(depth, platform)), '0' if platform == 'ios' else '1')
        self.assertFalse(self.enabled('Force qml-video-rs rebuild (UNIX)', 'ios'))
        self.assertFalse(self.enabled('Force qml-video-rs rebuild (Win)', 'ios'))
        self.assertFalse(self.enabled('Install Mac certificates', 'ios'))

    def test_only_ios_is_serialized_across_release_runs(self):
        group = re.search(r'^      group: (.+)$', self.build, re.MULTILINE).group(1)
        self.assertEqual(self.expression(group, 'ios', run=1), self.expression(group, 'ios', run=2))
        self.assertEqual(self.expression(group, 'ios'), 'niyien-ios-app-store-upload')
        others = [self.expression(group, platform, run=run)
                  for platform in self.platforms if platform != 'ios' for run in (1, 2)]
        self.assertEqual(len(others), len(set(others)))
        self.assertNotIn(self.expression(group, 'ios'), others)

    def test_github_release_does_not_download_ios_debug_artifact(self):
        release = self.workflow.split('\n  github_release:', 1)[1]
        pattern = re.search(r'pattern: (.+)', release).group(1)
        self.assertFalse(fnmatch.fnmatch('niyien-ios', pattern))
        assets = re.findall(r'\./release_artifacts/(\S+)', release)
        self.assertEqual(len(assets), 6)
        self.assertTrue(all(fnmatch.fnmatch(asset, pattern) for asset in assets))


if __name__ == '__main__':
    unittest.main()
