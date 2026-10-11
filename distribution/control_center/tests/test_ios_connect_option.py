# SPDX-License-Identifier: GPL-3.0-or-later
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

from distribution.control_center.backend import api as api_module
from distribution.control_center.backend import git as git_ops
from distribution.control_center.backend.api import Api

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / '_scripts'))
from ios_ci import UPLOAD_TAG_TRAILER, upload_requested


class IosConnectDispatchTests(unittest.TestCase):
    def setUp(self):
        self.github = Mock()
        self.api = Api()
        self.api._github = Mock(return_value=self.github)
        self.addCleanup(patch.stopall)
        patch.object(api_module.config_module, 'load_config', return_value={'git_remote': 'origin'}).start()
        patch.object(git_ops, 'get_current_branch', return_value='niyien').start()
        patch.object(git_ops, 'get_head_commit_sha', return_value='a' * 40).start()
        patch.object(git_ops, 'get_remote_branch_sha', return_value='a' * 40).start()
        patch.object(git_ops, 'local_tag_exists', return_value=False).start()
        patch.object(git_ops, 'remote_tag_exists', return_value=False).start()
        patch.object(api_module, '_bump_cargo_and_commit_if_needed', return_value=None).start()

    def test_checked_and_unchecked_builds_forward_explicit_input(self):
        for selected in (False, True):
            self.github.reset_mock()
            result = self.api.trigger_action_build('test build', selected)
            self.assertTrue(result['ok'], result)
            calls = self.github.dispatch_workflow.call_args_list
            self.assertEqual(calls[0].args, ('release.yml', 'niyien'))
            self.assertEqual(calls[1].args, ('ios-release.yml', 'niyien'))
            self.assertEqual(calls[1].kwargs['inputs']['upload_to_connect'], str(selected).lower())

    def test_non_boolean_selection_never_triggers_workflow(self):
        self.assertFalse(self.api.trigger_action_build('test', 'false')['ok'])
        self.github.dispatch_workflow.assert_not_called()

    def test_remote_head_guard_remains_active(self):
        git_ops.get_remote_branch_sha.return_value = 'b' * 40
        self.assertFalse(self.api.trigger_action_build('test', True)['ok'])
        self.github.dispatch_workflow.assert_not_called()

    def test_ios_dispatch_failure_reports_partial_success(self):
        self.github.dispatch_workflow.side_effect = [True, RuntimeError('fixture failure')]
        result = self.api.trigger_action_build('test', True)
        self.assertFalse(result['ok'])
        self.assertTrue(result['app_dispatched'])
        self.assertIn('主程序编译已触发', result['error'])

    def test_tag_choice_is_immutable_and_does_not_dispatch_a_second_ios_build(self):
        with patch.object(git_ops, 'create_and_push_tag') as create:
            result = self.api.create_and_push_tag(1, 6, 4, True)
            self.assertTrue(result['ok'], result)
            self.assertIn(UPLOAD_TAG_TRAILER + ': true', create.call_args.kwargs['annotation'])
            self.github.dispatch_workflow.assert_not_called()
            create.reset_mock()
            self.assertTrue(self.api.create_and_push_tag(1, 6, 5, False)['ok'])
            self.assertEqual(create.call_args.args[1:], ('origin', 'v1.6.5'))
            self.assertFalse(create.call_args.kwargs)


class IosTagSelectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git('init', '-q')
        self.git('config', 'user.email', 'fixture@example.com')
        self.git('config', 'user.name', 'Fixture')
        self.git('commit', '--allow-empty', '-qm', UPLOAD_TAG_TRAILER + ': true')

    def git(self, *args):
        return subprocess.run(['git', *args], cwd=self.root, check=True, capture_output=True, text=True)

    def selection(self, tag, event='push', checked='false'):
        return upload_requested({'GITHUB_REF': 'refs/tags/' + tag, 'GITHUB_EVENT_NAME': event,
                                 'IOS_UPLOAD_TO_CONNECT': checked}, self.root)

    def test_plain_tag_never_uses_a_marker_from_the_commit_message(self):
        self.git('tag', 'v1.0.0')
        self.assertFalse(self.selection('v1.0.0'))

    def test_annotated_tag_carries_the_ui_choice_to_the_workflow(self):
        with patch.object(git_ops, 'run_git', wraps=git_ops.run_git) as commands:
            commands.side_effect = lambda workdir, *args, **kwargs: (
                subprocess.CompletedProcess([], 0) if args[0] == 'push' else self.git(*args)
            )
            git_ops.create_and_push_tag(self.root, 'origin', 'v1.0.1', annotation='release\n\n' + UPLOAD_TAG_TRAILER + ': true')
        self.assertTrue(self.selection('v1.0.1'))
        self.assertFalse(self.selection('v1.0.1', event='workflow_dispatch', checked='false'))

    def test_ambiguous_tag_upload_choices_are_rejected(self):
        self.git('tag', '-a', 'v1.0.2', '-m', UPLOAD_TAG_TRAILER + ': true\n' + UPLOAD_TAG_TRAILER + ': false')
        with self.assertRaises(ValueError):
            self.selection('v1.0.2')


class IosConnectFrontendTests(unittest.TestCase):
    def test_frontend_checkbox_forwards_both_values_to_build_and_tag(self):
        script = r"""
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const source = fs.readFileSync(process.argv[1], 'utf8');
const helperStart = source.indexOf('function iosUploadToConnectSelected()');
const triggerEnd = source.indexOf('// Prefill publish view', helperStart);
const tagStart = source.indexOf("document.getElementById('push-tag-btn')?.addEventListener");
const tagEnd = source.indexOf('// ---- Mode 3:', tagStart);
const code = source.slice(helperStart, triggerEnd) + source.slice(tagStart, tagEnd);
(async () => {
  for (const checked of [false, true]) {
    const handlers = {};
    const calls = [];
    const elements = new Proxy({}, { get: (target, id) => target[id] ||= {
      value: ({'trigger-build-label': ' test build ', 'tag-major': '1', 'tag-minor': '6', 'tag-patch': '4'})[id] || '',
      checked: id === 'ios-upload-to-connect' && checked,
      addEventListener: (_, handler) => { handlers[id] = handler; }
    } });
    const context = {
      document: { getElementById: id => elements[id] },
      confirm: text => { assert.ok(text.includes(checked ? '上传' : '仅编译')); return true; },
      pywebview: { api: {
        trigger_action_build: async (...args) => { calls.push(args); return {ok:true, label:'test'}; },
        create_and_push_tag: async (...args) => { calls.push(args); return {ok:true, tag:'v1.6.4'}; }
      } }
    };
    vm.createContext(context);
    vm.runInContext(code, context);
    await handlers['trigger-action-btn']();
    await handlers['push-tag-btn']();
    assert.deepEqual(calls, [['test build', checked], [1, 6, 4, checked]]);
  }
})().catch(error => { console.error(error); process.exit(1); });
"""
        subprocess.run(['node', '-e', script, str(ROOT / 'distribution/control_center/frontend/app.js')], check=True)

    def test_checkbox_is_not_preselected(self):
        from html.parser import HTMLParser
        class Inputs(HTMLParser):
            checkbox = None
            def handle_starttag(self, tag, attrs):
                attrs = dict(attrs)
                if tag == 'input' and attrs.get('id') == 'ios-upload-to-connect':
                    self.checkbox = attrs
        parsed = Inputs()
        parsed.feed((ROOT / 'distribution/control_center/frontend/index.html').read_text(encoding='utf-8'))
        self.assertIsNotNone(parsed.checkbox)
        self.assertNotIn('checked', parsed.checkbox)


if __name__ == '__main__':
    unittest.main()
