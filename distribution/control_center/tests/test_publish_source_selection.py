import base64
import subprocess
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from distribution.control_center.backend.api import Api


APP_JS = Path(__file__).resolve().parents[1] / "frontend" / "app.js"


class BuildBaseVersionTests(unittest.TestCase):
    def test_reads_workspace_version_at_selected_commit(self):
        sha = "a" * 40
        gh = Mock()
        gh.get_contents.return_value = {
            "content": base64.b64encode(
                b'[package]\nversion = "0.0.1"\n[workspace.package]\nversion = "1.7.0"\n'
            ).decode("ascii")
        }
        with patch.object(Api, "_github", return_value=gh):
            result = Api().get_action_build_base_version(sha)
        self.assertEqual(result, {"ok": True, "version": "1.7.0"})
        gh.get_contents.assert_called_once_with(path="Cargo.toml", ref=sha)

    def test_rejects_unreadable_or_invalid_build_version(self):
        gh = Mock()
        with patch.object(Api, "_github", return_value=gh):
            self.assertFalse(Api().get_action_build_base_version("niyien")["ok"])
            gh.get_contents.assert_not_called()
            for text in (None, b"invalid TOML", b'[workspace.package]\nversion = "bad"'):
                with self.subTest(text=text):
                    gh.get_contents.return_value = None if text is None else {
                        "content": base64.b64encode(text).decode("ascii")
                    }
                    self.assertFalse(Api().get_action_build_base_version("a" * 40)["ok"])


class PublishSourceSelectionTests(unittest.TestCase):
    def test_source_versions_and_multilingual_notes_restore(self):
        script = r"""
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const source = fs.readFileSync(process.argv[1], 'utf8');
const code = source.slice(source.indexOf('const publishState ='), source.indexOf('async function checkTranslateAvailability'))
  + source.slice(source.indexOf('// Source type toggle'), source.indexOf('// ---- Pan123 sync progress polling'));
(async () => {
  const elements = new Map();
  const sourceItems = [];
  function element(id) {
    if (!elements.has(id)) elements.set(id, {
      value: '', textContent: '', innerHTML: '', disabled: false, dataset: {}, handlers: {},
      classList: { values: new Set(), add(name) { this.values.add(name); },
        remove(name) { this.values.delete(name); }, contains(name) { return this.values.has(name); },
        toggle(name, force) { force ? this.add(name) : this.remove(name); } },
      addEventListener(event, handler) { this.handlers[event] = handler; },
      querySelectorAll() { return []; }
    });
    return elements.get(id);
  }
  function item(kind, data) {
    const el = element('item-' + sourceItems.length);
    el.dataset = {kind, ...data};
    sourceItems.push(el);
    return el;
  }
  const langs = ['zh', 'en', 'ja', 'ko', 'de', 'fr', 'es', 'ru', 'pt'];
  const notes = Object.fromEntries(langs.map(lang => [lang, lang + ' saved notes']));
  const builds = [
    {run_id: 1660, run_number: 166, head_sha: 'a'.repeat(40), title: 'latest commit'},
    {run_id: 1610, run_number: 161, head_sha: 'b'.repeat(40), title: 'old commit'},
    {run_id: 1670, run_number: 167, head_sha: 'c'.repeat(40), title: 'new commit'},
  ];
  const latest = item('artifact', {runId: '1660'});
  const older = item('artifact', {runId: '1610'});
  const fresh = item('artifact', {runId: '1670'});
  const release = item('release', {tag: 'v1.7.0'});
  const releases = [{tag: 'v1.7.0', body: 'GitHub release title'}];
  let finishNotes;
  let notesResult = new Promise(resolve => { finishNotes = resolve; });
  const versionCalls = [];
  let versionResult = {ok: true, version: '1.8.0'};
  const context = {
    document: { getElementById: element, addEventListener() {},
      querySelectorAll: selector => selector === '.source-item' ? sourceItems : [] },
    console: {warn() {}},
    waitForApi: async () => {}, escapeHtml: value => String(value), fmtRelativeTime: () => '',
    clearReleasePlanOutcomeIfIdle() {}, refreshReleasePlanChecklist() {},
    releasePlanState: {},
    pywebview: {api: {
      list_action_builds: async () => ({ok: true, builds}),
      list_releases: async () => ({ok: true, releases}),
      list_policy_versions_changelog_map: () => notesResult,
      get_action_build_base_version: sha => { versionCalls.push(sha); return versionResult; },
    }}
  };
  vm.createContext(context);
  vm.runInContext(code, context);
  const state = vm.runInContext('publishState', context);
  const tabs = vm.runInContext('changelogTabsState', context);
  const map = {
    '1.7.0-ni.166': {changelog: 'saved English', changelogs: notes},
    '1.6.3-ni.161': {changelog: 'old notes', changelogs: {zh: 'old Chinese', en: 'old English'}},
    '1.7.0': {changelog: 'release notes', changelogs: notes},
  };

  const loading = context.loadSourceList('artifact');
  await new Promise(resolve => setImmediate(resolve));
  assert.ok(element('source-list').innerHTML.includes('加载中'));
  finishNotes({ok: true, map});
  await loading;
  await context.selectSourceItem(latest, builds);
  assert.equal(context.computeFinalVersion(), '1.7.0-ni.166');
  assert.equal(state.reuseSnapshotVersion, '1.7.0-ni.166');
  for (const lang of langs) assert.equal(tabs.texts[lang], notes[lang]);
  assert.equal(element('pub-changelog-active').value, notes.zh);
  assert.equal(versionCalls.length, 0);

  await context.selectSourceItem(release, releases);
  assert.equal(context.computeFinalVersion(), '1.7.0');
  assert.equal(tabs.texts.ja, notes.ja);
  await context.selectSourceItem(older, builds);
  assert.equal(context.computeFinalVersion(), '1.6.3-ni.161');
  assert.equal(tabs.texts.zh, 'old Chinese');
  assert.equal(tabs.texts.en, 'old English');
  assert.equal(tabs.texts.ja, '');

  await context.selectSourceItem(fresh, builds);
  assert.deepEqual(versionCalls, ['c'.repeat(40)]);
  assert.equal(context.computeFinalVersion(), '1.8.0-ni.167');
  assert.equal(tabs.texts.zh, 'new commit');
  assert.equal(state.reuseSnapshotVersion, '');
  assert.equal(element('execute-publish-btn').disabled, false);

  versionResult = {ok: false, error: 'GitHub offline'};
  await context.selectSourceItem(fresh, builds);
  assert.equal(state.selected, null);
  assert.equal(element('execute-publish-btn').disabled, true);
  assert.ok(element('execute-publish-result').textContent.includes('GitHub offline'));

  let finishVersion;
  versionResult = new Promise(resolve => { finishVersion = resolve; });
  const selecting = context.selectSourceItem(fresh, builds);
  assert.equal(element('execute-publish-btn').disabled, true);
  await context.selectSourceItem(release, releases);
  finishVersion({ok: true, version: '1.8.0'});
  await selecting;
  assert.equal(context.computeFinalVersion(), '1.7.0');
  assert.equal(tabs.texts.zh, notes.zh);

  notesResult = {ok: true, map: {...map, '1.7.0-ni.166': {changelogs: {zh: 'updated notes'}}}};
  await context.loadSourceList('artifact');
  await context.selectSourceItem(latest, builds);
  assert.equal(tabs.texts.zh, 'updated notes');

  notesResult = {ok: false, error: 'Vercel offline'};
  await context.loadSourceList('release');
  await context.selectSourceItem(release, releases);
  assert.equal(tabs.texts.zh, 'GitHub release title');
  assert.equal(element('pub-changelog-reuse-warning').classList.contains('hidden'), false);
  assert.ok(element('pub-changelog-reuse-warning').textContent.includes('加载失败'));
})().catch(error => { console.error(error); process.exit(1); });
"""
        subprocess.run(["node", "-e", script, str(APP_JS)], check=True)


if __name__ == "__main__":
    unittest.main()
