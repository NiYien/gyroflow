# SPDX-License-Identifier: GPL-3.0-or-later
from pathlib import Path
import subprocess
import unittest
from unittest.mock import Mock

from distribution.control_center.backend.api import Api

ROOT = Path(__file__).resolve().parents[3]


class ExistingIOSUploadTests(unittest.TestCase):
    def setUp(self):
        self.gh = Mock(owner='NiYien', repo='gyroflow')
        self.api = Api()
        self.api._github = Mock(return_value=self.gh)
        self.run = {'id': 12345, 'run_number': 150, 'path': '.github/workflows/release.yml',
                    'status': 'completed', 'conclusion': 'failure', 'event': 'workflow_dispatch',
                    'head_repository': {'full_name': 'NiYien/gyroflow'}, 'display_title': 'Fixture build',
                    'head_branch': 'niyien', 'head_sha': 'a' * 40, 'created_at': '2026-10-16T01:00:00Z'}
        self.artifact = {'id': 55, 'name': 'niyien-ios', 'expired': False, 'workflow_run': {'id': 12345}}
        self.gh.get_workflow_run.return_value = self.run
        self.gh.list_repo_artifacts.return_value = [self.artifact]
        self.gh.list_run_artifacts.return_value = [self.artifact]
        self.gh.get_repo.return_value = {'default_branch': 'niyien'}
        self.gh.list_workflows.return_value = [{'path': '.github/workflows/ios-upload.yml', 'state': 'active'}]

    def test_lists_completed_build_with_ipa_even_if_other_jobs_failed(self):
        result = self.api.list_ios_upload_builds()
        self.assertTrue(result['ok'], result)
        self.assertEqual([item['run_id'] for item in result['builds']], [12345])
        self.assertEqual(result['builds'][0]['run_number'], 150)
        self.gh.list_repo_artifacts.assert_called_once_with(name='niyien-ios', per_page=100)

    def test_skips_expired_other_artifacts_and_duplicate_runs(self):
        self.gh.list_repo_artifacts.return_value = [
            {**self.artifact, 'expired': True}, {**self.artifact, 'name': 'desktop'}, self.artifact, self.artifact]
        self.assertEqual(len(self.api.list_ios_upload_builds()['builds']), 1)
        self.gh.get_workflow_run.assert_called_once_with(run_id=12345)

    def test_list_and_dispatch_reject_wrong_source_or_unfinished_run(self):
        for overrides in [
            {'path': '.github/workflows/ios-upload.yml'}, {'status': 'in_progress'},
            {'event': 'pull_request'}, {'head_repository': {'full_name': 'someone/fork'}},
        ]:
            self.gh.get_workflow_run.return_value = {**self.run, **overrides}
            with self.subTest(overrides=overrides):
                self.assertEqual(self.api.list_ios_upload_builds()['builds'], [])
                self.assertFalse(self.api.upload_existing_ios_ipa(12345)['ok'])
        self.gh.dispatch_workflow.assert_not_called()

    def test_historical_ios_workflow_is_supported(self):
        self.run['path'] = '.github/workflows/ios-release.yml@refs/heads/niyien'
        self.assertEqual(len(self.api.list_ios_upload_builds()['builds']), 1)
        self.assertTrue(self.api.upload_existing_ios_ipa(12345)['ok'])

    def test_dispatch_uses_source_id_and_default_branch_without_new_build(self):
        result = self.api.upload_existing_ios_ipa(12345)
        self.assertTrue(result['ok'], result)
        self.gh.dispatch_workflow.assert_called_once_with('ios-upload.yml', 'niyien', inputs={'source_run_id': '12345'})
        self.assertIn('#150', result['message'])
        self.assertIn('不重新编译', result['message'])

    def test_rechecks_artifact_after_selection(self):
        self.assertTrue(self.api.list_ios_upload_builds()['builds'])
        for artifacts in [[], [{**self.artifact, 'expired': True}], [self.artifact, self.artifact]]:
            self.gh.list_run_artifacts.return_value = artifacts
            self.assertFalse(self.api.upload_existing_ios_ipa(12345)['ok'])
        self.gh.dispatch_workflow.assert_not_called()

    def test_invalid_id_never_contacts_github(self):
        for value in [None, True, 0, -1, '12345', 1.5, 2 ** 53]:
            self.assertFalse(self.api.upload_existing_ios_ipa(value)['ok'])
        self.api._github.assert_not_called()

    def test_missing_workflow_and_branch_report_actionable_errors(self):
        self.gh.list_workflows.return_value = []
        result = self.api.upload_existing_ios_ipa(12345)
        self.assertFalse(result['ok'])
        self.assertIn('推送到仓库默认分支', result['error'])
        self.gh.list_workflows.return_value = [{'path': '.github/workflows/ios-upload.yml', 'state': 'active'}]
        self.gh.get_repo.return_value = {}
        self.assertFalse(self.api.upload_existing_ios_ipa(12345)['ok'])
        self.gh.dispatch_workflow.assert_not_called()

    def test_dispatch_error_does_not_claim_upload_success_or_retry(self):
        self.gh.dispatch_workflow.side_effect = RuntimeError('fixture failure')
        result = self.api.upload_existing_ios_ipa(12345)
        self.assertFalse(result['ok'])
        self.gh.dispatch_workflow.assert_called_once()

    def test_upload_history_and_regular_build_list_stay_separate(self):
        upload = {**self.run, 'id': 999, 'path': '.github/workflows/ios-upload.yml', 'status': 'in_progress'}
        self.gh.list_repo_workflow_runs.return_value = [upload, self.run]
        result = self.api.list_ios_connect_uploads()
        self.assertEqual([item['run_id'] for item in result['uploads']], [999])
        self.assertEqual(result['uploads'][0]['status'], 'in_progress')
        self.assertEqual([item['run_id'] for item in self.api.list_action_builds()['builds']], [12345])


class ExistingIOSUploadFrontendTests(unittest.TestCase):
    def test_selection_submission_retry_status_and_mode_switch(self):
        script = r"""
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const source = fs.readFileSync(process.argv[1], 'utf8');
const start = source.indexOf('// Upload an existing iOS build');
const end = source.indexOf("document.querySelectorAll('.plan-plugin-mode-btn')", start);
const code = source.slice(start, end);
(async () => {
  const elements = {};
  function element(id) {
    return elements[id] ||= { value: '', disabled: false, textContent: '', innerHTML: '', children: [], handlers: {},
      classList: { toggle(name, value) { this[name] = value; } },
      addEventListener(event, handler) { this.handlers[event] = handler; },
      replaceChildren() { this.children = []; this.value = ''; },
      appendChild(child) { this.children.push(child); } };
  }
  const modes = ['trigger', 'tag', 'select', 'ios-upload'].map(mode => Object.assign(element(mode), {dataset: {mode}}));
  const panels = modes.map(mode => Object.assign(element('panel-' + mode.dataset.mode), {dataset: {modePanel: mode.dataset.mode}}));
  const build = {run_id: 12345, run_number: 150, title: '<img src=x onerror=bad()>', branch: 'niyien', created_at: 'today'};
  const calls = [];
  let buildResult = {ok: true, builds: [build]};
  let dispatchResolve;
  let uploads = [{title: 'Upload iOS IPA from run 12345', status: 'in_progress', created_at: 'today'}];
  const context = {
    document: {getElementById: element, createElement: () => ({value: '', textContent: ''}),
      querySelectorAll: selector => selector === '.mode-btn' ? modes : panels},
    publishState: {source: 'release'},
    fmtRelativeTime: () => '刚刚',
    escapeHtml: value => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;'),
    pywebview: {api: {
      list_ios_upload_builds: async () => buildResult,
      list_ios_connect_uploads: async () => ({ok: true, uploads}),
      upload_existing_ios_ipa: id => { calls.push(id); return new Promise(resolve => { dispatchResolve = resolve; }); }
    }}
  };
  vm.createContext(context);
  vm.runInContext(code, context);
  await context.loadIosUploadBuilds();
  assert.equal(element('ios-upload-build').children.length, 2);
  assert.ok(element('ios-upload-build').children[1].textContent.includes('#150'));
  assert.equal(element('ios-upload-submit').disabled, true);
  element('ios-upload-build').value = '12345';
  element('ios-upload-build').handlers.change();
  assert.equal(element('ios-upload-submit').disabled, false);
  const pending = element('ios-upload-submit').handlers.click();
  await element('ios-upload-submit').handlers.click();
  assert.deepEqual(calls, [12345]);
  assert.equal(element('ios-upload-build').disabled, true);
  dispatchResolve({ok: true, message: '已提交上传任务'});
  await pending;
  assert.equal(element('ios-upload-submit').disabled, true);
  assert.ok(element('ios-upload-result').textContent.includes('已提交'));
  assert.ok(element('ios-upload-history').innerHTML.includes('正在签名或上传'));
  assert.ok(element('ios-upload-history').innerHTML.includes('&lt;img'));
  assert.ok(!element('ios-upload-history').innerHTML.includes('<img'));
  uploads = [{title: 'test', status: 'completed', conclusion: 'failure'}];
  await context.loadIosUploadHistory();
  assert.ok(element('ios-upload-history').innerHTML.includes('上传失败'));
  await context.loadIosUploadBuilds();
  element('ios-upload-build').value = '12345';
  const retry = element('ios-upload-submit').handlers.click();
  dispatchResolve({ok: false, error: 'workflow unavailable'});
  await retry;
  assert.ok(element('ios-upload-result').textContent.includes('workflow unavailable'));
  assert.equal(element('ios-upload-submit').disabled, false);
  buildResult = {ok: false, error: 'network error'};
  await context.loadIosUploadBuilds();
  assert.ok(element('ios-upload-builds-status').textContent.includes('network error'));
  assert.equal(element('ios-upload-submit').disabled, true);
  buildResult = {ok: true, builds: []};
  await context.loadIosUploadBuilds();
  assert.ok(element('ios-upload-builds-status').textContent.includes('没有找到'));
  modes[3].handlers.click();
  assert.equal(element('ios-connect-option').classList.hidden, true);
  assert.equal(panels[3].classList.hidden, false);
  modes[0].handlers.click();
  assert.equal(element('ios-connect-option').classList.hidden, false);
})().catch(error => { console.error(error); process.exit(1); });
"""
        subprocess.run(['node', '-e', script, str(ROOT / 'distribution/control_center/frontend/app.js')], check=True)


if __name__ == '__main__':
    unittest.main()
