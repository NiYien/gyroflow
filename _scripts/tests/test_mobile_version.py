# SPDX-License-Identifier: GPL-3.0-or-later
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from ios_metadata import load_metadata
from mobile_version import load_version, prepare_ci_version, update_version


class MobileVersionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        deployment = self.root / '_deployment'
        (deployment / 'ios').mkdir(parents=True)
        (deployment / 'mobile-version.json').write_text('{"version":"1.0.0"}')
        (deployment / 'ios/app.json').write_text(json.dumps({'bundle_identifier': 'com.niyien.stabilizer',
            'display_name': 'NiYien', 'build_number': '4', 'minimum_os_version': '15.0'}))

    def test_skipping_versions_updates_ios_from_the_same_source(self):
        self.assertEqual(update_version(self.root, version='1.0.5'), '1.0.5')
        self.assertEqual(load_version(self.root), '1.0.5')
        self.assertEqual(load_metadata(self.root, {})['version'], '1.0.5')

    def test_bump_operations_increment_and_reset_lower_segments(self):
        for bump, expected in [('patch', '1.0.1'), ('minor', '1.1.0'), ('major', '2.0.0')]:
            self.assertEqual(update_version(self.root, bump=bump), expected)

    def test_invalid_or_decreasing_release_does_not_change_the_file(self):
        for version in ['1.0.0', '0.9.9', '1.0.0.1', '1.0.1-dev.2', 'invalid']:
            with self.subTest(version=version), self.assertRaises(ValueError):
                update_version(self.root, version=version)
            self.assertEqual(load_version(self.root), '1.0.0')

    def configure_ci(self, version='1.0.1', run=149):
        path = self.root / '_deployment/mobile-version.json'
        path.write_text(json.dumps({'version': version, 'ci_run_number': run}))
        return path

    def ci_environment(self, run, attempt=1):
        return {'GITHUB_ACTIONS': 'true', 'GITHUB_RUN_NUMBER': str(run), 'GITHUB_RUN_ATTEMPT': str(attempt)}

    def test_new_runs_increment_patch_on_fresh_runners(self):
        for run, expected in [(150, '1.0.2'), (151, '1.0.3'), (154, '1.0.6')]:
            with self.subTest(run=run):
                self.configure_ci()
                self.assertEqual(prepare_ci_version(self.root, self.ci_environment(run)), expected)

    def test_independent_android_and_ios_jobs_share_the_version(self):
        self.configure_ci()
        prepare_ci_version(self.root, self.ci_environment(150))
        android_version = json.loads((self.root / '_deployment/mobile-version.json').read_text())['version']
        self.configure_ci()
        prepare_ci_version(self.root, self.ci_environment(150))
        self.assertEqual(load_metadata(self.root, {})['version'], android_version)
        self.assertEqual(android_version, '1.0.2')

    def test_repeat_preparation_and_reruns_keep_the_display_version(self):
        path = self.configure_ci()
        prepare_ci_version(self.root, self.ci_environment(150))
        prepared = path.read_bytes()
        self.assertEqual(prepare_ci_version(self.root, self.ci_environment(150)), '1.0.2')
        self.assertEqual(path.read_bytes(), prepared)
        self.configure_ci()
        self.assertEqual(prepare_ci_version(self.root, self.ci_environment(150, attempt=2)), '1.0.2')
        self.assertEqual(path.read_bytes(), prepared)

    def test_tag_does_not_override_the_shared_mobile_sequence(self):
        self.configure_ci()
        environment = {**self.ci_environment(150), 'GITHUB_REF': 'refs/tags/v1.6.4', 'GITHUB_EVENT_NAME': 'push'}
        self.assertEqual(prepare_ci_version(self.root, environment), '1.0.2')

    def test_manual_bump_preserves_ci_baseline(self):
        path = self.configure_ci()
        self.assertEqual(update_version(self.root, bump='minor'), '1.1.0')
        self.assertEqual(json.loads(path.read_text())['ci_run_number'], 149)
        self.assertEqual(prepare_ci_version(self.root, self.ci_environment(150)), '1.1.1')

    def test_local_reads_do_not_advance_or_rewrite_the_version(self):
        path = self.configure_ci()
        original = path.read_bytes()
        self.assertEqual(load_version(self.root), '1.0.1')
        self.assertEqual(load_metadata(self.root, {})['version'], '1.0.1')
        self.assertEqual(path.read_bytes(), original)

    def test_invalid_ci_environment_does_not_change_the_file(self):
        path = self.configure_ci()
        original = path.read_bytes()
        for environment in [{}, {'GITHUB_RUN_NUMBER': '150'},
                            *[self.ci_environment(run) for run in ['', '0', '-1', 'bad', '150.1', '148']]]:
            with self.subTest(environment=environment), self.assertRaises(ValueError):
                prepare_ci_version(self.root, environment)
            self.assertEqual(path.read_bytes(), original)

    def test_missing_or_invalid_baseline_is_rejected_without_writing(self):
        for baseline in [None, True, '149', 0, -1]:
            path = self.configure_ci(run=baseline)
            original = path.read_bytes()
            with self.subTest(baseline=baseline), self.assertRaises(ValueError):
                prepare_ci_version(self.root, self.ci_environment(150))
            self.assertEqual(path.read_bytes(), original)


if __name__ == '__main__':
    unittest.main()
