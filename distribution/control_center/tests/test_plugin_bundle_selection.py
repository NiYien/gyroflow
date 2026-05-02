import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from _scripts import publish_pan123_release as publish_module
from distribution.control_center.backend import api as api_module
from distribution.control_center.backend.api import Api

REPO_ROOT = Path(__file__).resolve().parents[3]


class FakeVercel:
    def __init__(self):
        self.envs = {
            "NIYIEN_LENS_RELEASE_TAG": "lens-current",
            "NIYIEN_PLUGIN_RELEASE_TAG": "plugin-old",
            "NIYIEN_PLUGINS_RUN_ID": "88",
            "NIYIEN_RELEASE_POLICY_JSON": """{
  "auto_version": "1.0.0",
  "versions": [
    {
      "version": "1.0.0",
      "tag": "run-2",
      "channels": ["auto", "manual"]
    }
  ]
}""",
        }
        self.upserts = []
        self.list_calls = 0

    def list_envs_decrypted(self):
        self.list_calls += 1
        return dict(self.envs)

    def upsert_envs(self, mapping):
        self.upserts.append(dict(mapping))
        self.envs.update(mapping)
        return {"ok": True}


class FakeGithub:
    def __init__(self):
        self.workflow_runs = []
        self.artifacts_by_run = {}
        self.workflow_run_calls = []
        self.run_artifact_calls = []

    def list_repo_workflow_runs(
        self,
        owner=None,
        repo=None,
        *,
        branch="",
        per_page=20,
        status="completed",
    ):
        self.workflow_run_calls.append(
            {
                "owner": owner,
                "repo": repo,
                "branch": branch,
                "per_page": per_page,
                "status": status,
            }
        )
        return self.workflow_runs[:per_page]

    def list_run_artifacts(self, owner=None, repo=None, run_id=0):
        self.run_artifact_calls.append({"owner": owner, "repo": repo, "run_id": run_id})
        return list(self.artifacts_by_run.get(int(run_id), []))

    def list_repo_artifacts(self, owner, repo, *, name, per_page):
        return [
            {
                "expired": False,
                "workflow_run": {"id": 2},
            }
        ]


class FakeApi(Api):
    def __init__(self):
        self.vercel = FakeVercel()
        self.github = FakeGithub()

    def _vercel(self, cfg):
        return self.vercel

    def _gh_for(self, owner, repo, cfg):
        return self.github

    def _fetch_lens_metadata_for_tag(self, cfg, lens_tag):
        return {}

    def _resolve_plugin_bundle_tag_for_source_ref(self, *, cfg, vercel_envs, current_tag, target_source_ref):
        self.resolved_source_ref = target_source_ref
        return "plugin-new"

    def _trigger_deploy_hook(self, cfg):
        return "deploy hook skipped"


class FailingResolveApi(FakeApi):
    def _resolve_plugin_bundle_tag_for_source_ref(self, *, cfg, vercel_envs, current_tag, target_source_ref):
        raise RuntimeError("bundle missing")


class FakePolicyVercel:
    def __init__(self):
        self.envs = {
            "NIYIEN_LENS_DATA_TAG": "data-v1",
            "NIYIEN_LENS_RELEASE_TAG": "lens-old",
            "NIYIEN_LENS_VERSION": "9",
            "NIYIEN_LENS_SHA256": "lenssha",
            "NIYIEN_PLUGINS_SOURCE_MODE": "release",
            "NIYIEN_PLUGINS_TAG": "v2.0.0",
            "NIYIEN_PLUGINS_ARTIFACT_NAME": "",
            "NIYIEN_PLUGINS_RUN_ID": "",
            "NIYIEN_PLUGIN_RELEASE_TAG": "plugin-live",
            "NIYIEN_SDK_BASE": "https://download.example.test/sdk/",
        }
        self.policy_json = """{
  "auto_version": "1.0.0",
  "versions": [
    {
      "version": "1.0.0",
      "tag": "v1.0.0",
      "channels": ["auto", "manual"]
    }
  ]
}"""
        self.upserts = []

    def list_env_records(self):
        return {"NIYIEN_RELEASE_POLICY_JSON": {"id": "policy-env"}}

    def get_env_value(self, env_id):
        if env_id != "policy-env":
            raise RuntimeError("unexpected env id")
        return self.policy_json

    def list_envs_decrypted(self):
        return {**self.envs, "NIYIEN_RELEASE_POLICY_JSON": self.policy_json}

    def upsert_envs(self, mapping):
        self.upserts.append(dict(mapping))
        self.envs.update(mapping)
        if "NIYIEN_RELEASE_POLICY_JSON" in mapping:
            self.policy_json = mapping["NIYIEN_RELEASE_POLICY_JSON"]
        return {"ok": True}


class ReleasePlanApi(Api):
    def __init__(self, pan123_result=None):
        self.vercel = FakePolicyVercel()
        self.pan123_result = pan123_result or {"ok": True, "token": "plan-token"}
        self.pan123_calls = []
        self.app_action_calls = []
        self.captured_finalize = None

    def _vercel(self, cfg):
        return self.vercel

    def _trigger_deploy_hook(self, cfg):
        return "deploy hook skipped"

    def _start_pan123_publish(self, **kwargs):
        self.pan123_calls.append(kwargs)
        self.captured_finalize = kwargs.get("on_finalize")
        return dict(self.pan123_result)

    def execute_app_action(self, payload):
        self.app_action_calls.append(dict(payload))
        return {"ok": True, "message": "app action stub", "deploy_hook": "deploy hook skipped"}


class PluginBundleSelectionTests(unittest.TestCase):
    def setUp(self):
        self._orig_load_config = api_module.config_module.load_config
        api_module.config_module.load_config = lambda: {
            "github_owner": "NiYien",
            "github_repo": "gyroflow",
            "lens_data_owner": "NiYien",
            "lens_data_repo": "niyien-lens-data",
            "plugins_owner": "NiYien",
            "plugins_repo": "gyroflow-plugins",
            "network_proxy": "",
        }

    def tearDown(self):
        api_module.config_module.load_config = self._orig_load_config

    def test_selects_matching_artifact_bundle_instead_of_current_mismatch(self):
        selected = Api._select_plugin_bundle_tag_for_source_ref(
            [
                {
                    "tag": "plugin-old",
                    "plugin_source_ref": "actions-run-1",
                    "complete": True,
                },
                {
                    "tag": "plugin-new",
                    "plugin_source_ref": "actions-run-2",
                    "complete": True,
                },
            ],
            current_tag="plugin-old",
            target_source_ref="actions-run-2",
        )

        self.assertEqual(selected, "plugin-new")

    def test_rejects_artifact_switch_when_no_complete_bundle_matches(self):
        with self.assertRaisesRegex(RuntimeError, "actions-run-2"):
            Api._select_plugin_bundle_tag_for_source_ref(
                [
                    {
                        "tag": "plugin-old",
                        "plugin_source_ref": "actions-run-1",
                        "complete": True,
                    },
                    {
                        "tag": "plugin-incomplete",
                        "plugin_source_ref": "actions-run-2",
                        "complete": False,
                    },
                ],
                current_tag="plugin-old",
                target_source_ref="actions-run-2",
            )

    def test_reject_message_includes_manifest_read_error(self):
        with self.assertRaisesRegex(RuntimeError, "download_info failed"):
            Api._select_plugin_bundle_tag_for_source_ref(
                [
                    {
                        "tag": "plugin-broken",
                        "plugin_source_ref": "",
                        "complete": True,
                        "manifest_error": "download_info failed",
                    },
                ],
                current_tag="",
                target_source_ref="actions-run-2",
            )

    def test_plugin_bundle_scan_fails_closed_without_expected_filenames(self):
        original = api_module.EXPECTED_PLUGIN_FILENAMES
        api_module.EXPECTED_PLUGIN_FILENAMES = ()
        try:
            with self.assertRaisesRegex(RuntimeError, "EXPECTED_PLUGIN_FILENAMES"):
                Api()._list_plugin_bundle_sources(object(), 123)
        finally:
            api_module.EXPECTED_PLUGIN_FILENAMES = original

    def test_apply_resources_now_batches_policy_and_resource_envs(self):
        api = FakeApi()

        result = api.apply_resources_now(
            {
                "lens_tag": "data-v1",
                "plugin_mode": "artifact",
                "plugin_artifact_name": "GyroflowNiyien-Adobe-macos-zip",
                "sdk_base": "https://example.test/sdk/",
            }
        )

        self.assertTrue(result["ok"])
        self.assertEqual(api.resolved_source_ref, "actions-run-2")
        self.assertEqual(len(api.vercel.upserts), 1)
        self.assertEqual(api.vercel.list_calls, 1)
        upsert = api.vercel.upserts[0]
        self.assertEqual(upsert["NIYIEN_PLUGIN_RELEASE_TAG"], "plugin-new")
        self.assertIn("NIYIEN_RELEASE_POLICY_JSON", upsert)
        self.assertIn('"plugin_tag": "plugin-new"', upsert["NIYIEN_RELEASE_POLICY_JSON"])
        self.assertIn('"plugins_source_ref": "actions-run-2"', upsert["NIYIEN_RELEASE_POLICY_JSON"])

    def test_manual_resource_finalize_clears_disabled_resource_flags(self):
        api = FakeApi()

        note = api._finalize_resource_envs_to_vercel(
            {},
            {
                "scope": ["lens", "plugin"],
                "lens_tag": "lens-new",
                "plugin_tag": "plugin-new",
                "lens_version": 10,
                "lens_sha256": "newsha",
                "global_sdk_base": "https://download.example.test/sdk/",
            },
        )

        self.assertIn("deploy hook skipped", note)
        upsert = api.vercel.upserts[0]
        self.assertEqual(upsert["NIYIEN_LENS_RELEASE_TAG"], "lens-new")
        self.assertEqual(upsert["NIYIEN_LENS_DISABLED"], "")
        self.assertEqual(upsert["NIYIEN_PLUGIN_RELEASE_TAG"], "plugin-new")
        self.assertEqual(upsert["NIYIEN_PLUGINS_DISABLED"], "")
        self.assertEqual(upsert["NIYIEN_SDK_BASE"], "https://download.example.test/sdk/")
        self.assertEqual(upsert["NIYIEN_SDK_DISABLED"], "")

    def test_app_only_finalize_does_not_unhide_sdk_from_global_sdk_base(self):
        api = FakeApi()

        note = api._finalize_resource_envs_to_vercel(
            {},
            {
                "scope": ["app"],
                "global_sdk_base": "https://download.example.test/sdk/",
            },
        )

        self.assertIn("no resource env", note)
        self.assertEqual(api.vercel.upserts, [])

    def test_list_plugin_action_builds_returns_run_metadata_and_artifact_names(self):
        api = FakeApi()
        api.github.workflow_runs = [
            {
                "id": 101,
                "run_number": 17,
                "name": "Build plugins",
                "display_title": "Package updated plugins",
                "head_branch": "main",
                "head_sha": "abc123",
                "status": "completed",
                "conclusion": "success",
                "html_url": "https://github.example/runs/101",
                "created_at": "2026-05-01T01:02:03Z",
            },
            {
                "id": 100,
                "run_number": 16,
                "name": "Build plugins",
                "display_title": "",
                "head_commit": {"message": "Fallback commit title\n\nbody"},
                "head_branch": "main",
                "head_sha": "def456",
                "status": "in_progress",
                "conclusion": "",
                "html_url": "https://github.example/runs/100",
                "created_at": "2026-04-30T01:02:03Z",
            },
        ]
        api.github.artifacts_by_run = {
            101: [
                {"name": "GyroflowNiyien-Adobe-macos-zip", "expired": False},
                {"name": "GyroflowNiyien-frei0r-windows", "expired": False},
                {"name": "expired-artifact", "expired": True},
            ],
            100: [
                {"name": "GyroflowNiyien-OFX-linux", "expired": False},
            ],
        }

        result = api.list_plugin_action_builds(limit=2)

        self.assertTrue(result["ok"])
        self.assertEqual(api.github.workflow_run_calls[0]["owner"], "NiYien")
        self.assertEqual(api.github.workflow_run_calls[0]["repo"], "gyroflow-plugins")
        self.assertEqual(api.github.workflow_run_calls[0]["status"], "")
        self.assertEqual(len(result["builds"]), 2)
        first = result["builds"][0]
        self.assertEqual(first["run_id"], 101)
        self.assertEqual(first["run_number"], 17)
        self.assertEqual(first["title"], "Package updated plugins")
        self.assertEqual(first["branch"], "main")
        self.assertEqual(first["status"], "completed")
        self.assertEqual(first["conclusion"], "success")
        self.assertEqual(
            first["artifact_names"],
            ["GyroflowNiyien-Adobe-macos-zip", "GyroflowNiyien-frei0r-windows"],
        )
        self.assertEqual(
            first["artifact_name"],
            "GyroflowNiyien-Adobe-macos-zip,GyroflowNiyien-frei0r-windows",
        )
        self.assertEqual(result["builds"][1]["title"], "Fallback commit title")

    def test_resolve_plugin_source_prefers_explicit_run_id_over_latest_same_artifact_name(self):
        class ScriptFakeGithub:
            def __init__(self):
                self.run_artifact_calls = []

            def get_repository(self, owner, repo):
                return {"default_branch": "main"}

            def list_workflow_runs(self, owner, repo, branch="", per_page=20):
                return [
                    {"id": 200, "conclusion": "success"},
                    {"id": 199, "conclusion": "success"},
                ]

            def list_workflow_run_artifacts(self, owner, repo, run_id):
                self.run_artifact_calls.append(int(run_id))
                return [
                    {"name": "GyroflowNiyien-Plugin-linux", "expired": False},
                ]

        github = ScriptFakeGithub()
        tmpdir = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: shutil.rmtree(tmpdir, ignore_errors=True))

        with mock.patch.object(
            publish_module,
            "resolve_plugin_assets_from_artifacts",
            return_value=[],
        ):
            source = publish_module.resolve_plugin_source(
                github=github,
                temp_root=tmpdir,
                owner="NiYien",
                repo="gyroflow-plugins",
                source_mode="artifact",
                tag="",
                artifact_name="GyroflowNiyien-Plugin-linux",
                run_id=199,
            )

        self.assertEqual(source.run_id, 199)
        self.assertEqual(source.source_ref, "actions-run-199")
        self.assertEqual(github.run_artifact_calls, [199])

    def test_build_pan123_publish_command_carries_plugin_run_id(self):
        api = FakeApi()
        command = api._build_pan123_publish_command(
            app_tag="run-55",
            cfg={
                "github_owner": "NiYien",
                "github_repo": "gyroflow",
                "lens_data_owner": "NiYien",
                "lens_data_repo": "niyien-lens-data",
                "plugins_owner": "NiYien",
                "plugins_repo": "gyroflow-plugins",
                "publish_defaults": {},
            },
            vercel_envs={
                "NIYIEN_LENS_DATA_TAG": "data-v1",
                "NIYIEN_PLUGINS_SOURCE_MODE": "artifact",
                "NIYIEN_PLUGINS_TAG": "",
                "NIYIEN_PLUGINS_ARTIFACT_NAME": "same-bundle",
                "NIYIEN_SDK_BASE": "https://download.example.test/sdk/",
            },
            output_dir=Path("C:/tmp/publish"),
            app_run_id=55,
            scope=["app", "plugin"],
            publish_overrides={
                "plugin_mode": "artifact",
                "plugin_artifact_name": "same-bundle",
                "plugin_run_id": 199,
            },
        )

        self.assertIn("--plugins-run-id", command)
        self.assertIn("199", command)

    def test_get_resources_state_returns_plugin_run_id(self):
        result = FakeApi().get_resources_state()

        self.assertTrue(result["ok"])
        self.assertEqual(result["current"]["NIYIEN_PLUGINS_RUN_ID"], "88")

    def test_parse_args_treats_invalid_plugin_run_id_env_as_zero(self):
        with mock.patch.dict("os.environ", {"NIYIEN_PLUGINS_RUN_ID": "not-a-number"}):
            with mock.patch("sys.argv", ["publish_pan123_release.py"]):
                args = publish_module.parse_args()

        self.assertEqual(args.plugins_run_id, 0)

    def test_apply_resources_now_does_not_write_envs_when_bundle_resolution_fails(self):
        api = FailingResolveApi()

        result = api.apply_resources_now(
            {
                "lens_tag": "data-v1",
                "plugin_mode": "artifact",
                "plugin_artifact_name": "GyroflowNiyien-Adobe-macos-zip",
                "sdk_base": "https://example.test/sdk/",
            }
        )

        self.assertFalse(result["ok"])
        self.assertIn("bundle missing", result["error"])
        self.assertEqual(api.vercel.upserts, [])

    def test_release_plan_defers_manifest_until_pan123_success_and_uses_plan_resources(self):
        api = ReleasePlanApi()

        result = api.execute_release_plan(
            {
                "action": "publish_and_push",
                "source_kind": "artifact",
                "version": "1.2.3-0.ni.7",
                "run_id": 55,
                "changelog": "release notes",
                "recommended": True,
                "resources": {
                    "lens_tag": "data-v20260501.1",
                    "plugin_mode": "artifact",
                    "plugin_artifact_name": "GyroflowNiyien-Adobe-macos-zip",
                    "plugin_run_id": 88,
                    "sdk_base": "https://download.example.test/sdk/",
                },
            }
        )

        self.assertTrue(result["ok"])
        self.assertTrue(result["staged_until_pan123"])
        self.assertEqual(api.vercel.upserts, [])
        self.assertEqual(len(api.pan123_calls), 1)
        call = api.pan123_calls[0]
        self.assertEqual(call["app_tag"], "run-55")
        self.assertEqual(call["app_version"], "1.2.3-0.ni.7")
        self.assertEqual(call["app_run_id"], 55)
        self.assertEqual(call["scope"], ["app", "lens", "plugin"])
        self.assertEqual(
            call["publish_overrides"],
            {
                "lens_tag": "data-v20260501.1",
                "plugin_mode": "artifact",
                "plugin_tag": "",
                "plugin_artifact_name": "GyroflowNiyien-Adobe-macos-zip",
                "plugin_run_id": "88",
                "sdk_base": "https://download.example.test/sdk/",
            },
        )

        note = api.captured_finalize(
            {
                "lens_tag": "lens-hash",
                "lens_release_tag": "data-v20260501.1",
                "lens_version": 9,
                "lens_sha256": "abc123",
                "plugin_tag": "plugin-hash",
                "plugin_source_mode": "artifact",
                "plugin_source_ref": "actions-run-88",
                "plugin_source_tag": "GyroflowNiyien-Adobe-macos-zip",
                "packages": {"windows": {"installer_sha256": "sha"}},
            }
        )

        self.assertIn("deploy hook skipped", note)
        self.assertEqual(len(api.vercel.upserts), 1)
        upsert = api.vercel.upserts[0]
        self.assertEqual(upsert["NIYIEN_LENS_DATA_TAG"], "data-v20260501.1")
        self.assertEqual(upsert["NIYIEN_LENS_DISABLED"], "")
        self.assertEqual(upsert["NIYIEN_PLUGINS_SOURCE_MODE"], "artifact")
        self.assertEqual(upsert["NIYIEN_PLUGINS_TAG"], "")
        self.assertEqual(upsert["NIYIEN_PLUGINS_ARTIFACT_NAME"], "GyroflowNiyien-Adobe-macos-zip")
        self.assertEqual(upsert["NIYIEN_PLUGINS_DISABLED"], "")
        self.assertEqual(upsert["NIYIEN_SDK_BASE"], "https://download.example.test/sdk/")
        self.assertEqual(upsert["NIYIEN_SDK_DISABLED"], "")
        policy_json = upsert["NIYIEN_RELEASE_POLICY_JSON"]
        self.assertIn('"auto_version": "1.2.3-0.ni.7"', policy_json)
        self.assertIn('"tag": "run-55"', policy_json)
        self.assertIn('"lens_tag": "lens-hash"', policy_json)
        self.assertIn('"plugin_tag": "plugin-hash"', policy_json)
        self.assertIn('"packages"', policy_json)

    def test_release_plan_manual_only_uploads_pan123_before_manifest(self):
        api = ReleasePlanApi()

        result = api.execute_release_plan(
            {
                "action": "manual_only",
                "source_kind": "release",
                "version": "1.2.3",
                "tag": "v1.2.3",
                "resources": {
                    "lens_tag": "data-v20260501.1",
                    "plugin_mode": "release",
                    "plugin_tag": "v1.2.3",
                    "sdk_base": "https://download.example.test/sdk/",
                },
            }
        )

        self.assertTrue(result["ok"])
        self.assertTrue(result["staged_until_pan123"])
        self.assertEqual(api.app_action_calls, [])
        self.assertEqual(api.vercel.upserts, [])
        self.assertEqual(len(api.pan123_calls), 1)
        self.assertEqual(api.pan123_calls[0]["scope"], ["app", "lens", "plugin"])

        note = api.captured_finalize(
            {
                "lens_tag": "lens-hash",
                "plugin_tag": "plugin-hash",
            }
        )

        self.assertIn("deploy hook skipped", note)
        self.assertEqual(len(api.vercel.upserts), 1)
        policy_json = api.vercel.upserts[0]["NIYIEN_RELEASE_POLICY_JSON"]
        self.assertIn('"version": "1.2.3"', policy_json)
        self.assertIn('"channels": [\n        "manual"\n      ]', policy_json)
        self.assertIn('"auto_version": "1.0.0"', policy_json)

    def test_legacy_app_action_manual_only_uses_release_plan_upload(self):
        api = ReleasePlanApi()

        result = Api.execute_app_action(
            api,
            {
                "action": "manual_only",
                "source_kind": "release",
                "version": "1.2.3",
                "tag": "v1.2.3",
                "resources": {
                    "lens_tag": "data-v20260501.1",
                    "plugin_mode": "release",
                    "plugin_tag": "v1.2.3",
                    "sdk_base": "https://download.example.test/sdk/",
                },
            },
        )

        self.assertTrue(result["ok"])
        self.assertTrue(result["staged_until_pan123"])
        self.assertEqual(api.vercel.upserts, [])
        self.assertEqual(len(api.pan123_calls), 1)
        self.assertEqual(api.pan123_calls[0]["scope"], ["app", "lens", "plugin"])

    def test_release_plan_skips_empty_resource_envs_outside_requested_scope(self):
        api = ReleasePlanApi()

        result = api.execute_release_plan(
            {
                "action": "publish_and_push",
                "source_kind": "release",
                "version": "1.2.3",
                "tag": "v1.2.3",
                "scope": ["app"],
                "resources": {
                    "lens_tag": "",
                    "plugin_mode": "release",
                    "plugin_tag": "",
                    "plugin_artifact_name": "",
                    "sdk_base": "",
                },
            }
        )

        self.assertTrue(result["ok"])
        note = api.captured_finalize({})
        self.assertIn("deploy hook skipped", note)
        self.assertEqual(len(api.vercel.upserts), 1)
        upsert = api.vercel.upserts[0]
        self.assertNotIn("NIYIEN_LENS_DATA_TAG", upsert)
        self.assertNotIn("NIYIEN_PLUGINS_SOURCE_MODE", upsert)
        self.assertNotIn("NIYIEN_PLUGINS_TAG", upsert)
        self.assertNotIn("NIYIEN_PLUGINS_ARTIFACT_NAME", upsert)
        self.assertNotIn("NIYIEN_SDK_BASE", upsert)

    def test_release_plan_honors_sdk_selection(self):
        api = ReleasePlanApi()

        result = api.execute_release_plan(
            {
                "action": "publish_and_push",
                "source_kind": "release",
                "version": "1.2.3",
                "tag": "v1.2.3",
                "scope": ["app", "lens"],
                "resources": {
                    "lens_tag": "data-v20260501.1",
                    "plugin_mode": "release",
                    "plugin_tag": "",
                    "sdk_base": "https://download.example.test/sdk/",
                    "include_sdk": False,
                },
            }
        )

        self.assertTrue(result["ok"])
        self.assertEqual(api.pan123_calls[0]["publish_overrides"]["sdk_base"], "")
        note = api.captured_finalize({})
        self.assertIn("deploy hook skipped", note)
        self.assertNotIn("NIYIEN_SDK_BASE", api.vercel.upserts[0])

    def test_distribution_policy_normalizer_preserves_resource_fields_for_manifest(self):
        script = """
const { loadReleasePolicy } = require('./api/_distribution');
process.env.NIYIEN_RELEASE_POLICY_JSON = JSON.stringify({
  auto_version: '2.0.0',
  versions: [{
    version: '2.0.0',
    tag: 'v2.0.0',
    channels: ['auto', 'manual'],
    lens_tag: 'lens-live',
    lens_release_tag: 'data-v2',
    lens_version: 9,
    lens_sha256: 'lenssha',
    plugin_tag: 'plugin-live',
    plugins_release_tag: 'v2.0.0',
    plugins_source_mode: 'release',
    plugins_source_ref: 'v2.0.0',
    plugins_source_tag: 'v2.0.0',
    global_plugins_base: 'https://example.test/plugins/',
    global_sdk_base: 'https://example.test/sdk/'
  }]
});
const entry = loadReleasePolicy().versions[0];
const expected = {
  lens_tag: 'lens-live',
  lens_release_tag: 'data-v2',
  lens_version: 9,
  lens_sha256: 'lenssha',
  plugin_tag: 'plugin-live',
  plugins_release_tag: 'v2.0.0',
  global_plugins_base: 'https://example.test/plugins/',
  global_sdk_base: 'https://example.test/sdk/'
};
for (const [key, value] of Object.entries(expected)) {
  if (entry[key] !== value) {
    throw new Error(`${key} was ${entry[key]}`);
  }
}
"""
        subprocess.run(["node", "-e", script], cwd=REPO_ROOT, check=True)

    def test_manifest_honors_hidden_resource_flags_without_falling_back(self):
        script = """
const handler = require('./api/manifest');
process.env.NIYIEN_RELEASE_POLICY_JSON = JSON.stringify({
  auto_version: '2.0.0',
  versions: [{
    version: '2.0.0',
    tag: 'v2.0.0',
    channels: ['auto', 'manual']
  }]
});
process.env.NIYIEN_LENS_DISABLED = '1';
process.env.NIYIEN_PLUGINS_DISABLED = '1';
process.env.NIYIEN_SDK_DISABLED = '1';
const req = {
  query: { country: 'CN', platform: 'windows' },
  headers: { host: 'www.niyien.com', 'x-forwarded-proto': 'https' },
  socket: {}
};
const res = {
  statusCode: 0,
  headers: {},
  setHeader(key, value) { this.headers[key] = value; },
  status(code) { this.statusCode = code; return this; },
  json(payload) { this.payload = payload; }
};
handler(req, res).then(() => {
  if (res.payload.lens.url !== '') throw new Error(`lens url: ${res.payload.lens.url}`);
  if (res.payload.lens.version !== 0) throw new Error(`lens version: ${res.payload.lens.version}`);
  if (res.payload.plugins_base !== '') throw new Error(`plugins_base: ${res.payload.plugins_base}`);
  if (res.payload.sdk_base !== '') throw new Error(`sdk_base: ${res.payload.sdk_base}`);
}).catch((err) => {
  console.error(err);
  process.exit(1);
});
"""
        subprocess.run(["node", "-e", script], cwd=REPO_ROOT, check=True)

    def test_manifest_uses_policy_plugin_tag_after_hide_app_only_promotes_auto(self):
        script = """
const handler = require('./api/manifest');
process.env.NIYIEN_RELEASE_POLICY_JSON = JSON.stringify({
  auto_version: '1.0.0',
  versions: [{
    version: '1.0.0',
    tag: 'v1.0.0',
    channels: ['auto', 'manual'],
    plugin_tag: 'plugin-live'
  }]
});
delete process.env.NIYIEN_PLUGIN_RELEASE_TAG;
delete process.env.NIYIEN_PLUGINS_DISABLED;
const req = {
  query: { country: 'CN', platform: 'windows' },
  headers: { host: 'www.niyien.com', 'x-forwarded-proto': 'https' },
  socket: {}
};
const res = {
  setHeader() {},
  status() { return this; },
  json(payload) { this.payload = payload; }
};
handler(req, res).then(() => {
  if (!res.payload.plugins_base.includes('/plugin-live/')) {
    throw new Error(`plugins_base: ${res.payload.plugins_base}`);
  }
}).catch((err) => {
  console.error(err);
  process.exit(1);
});
"""
        subprocess.run(["node", "-e", script], cwd=REPO_ROOT, check=True)

    def test_manifest_preserves_legacy_plugin_fallback_slash_in_cn_base(self):
        script = """
const handler = require('./api/manifest');
process.env.NIYIEN_RELEASE_POLICY_JSON = JSON.stringify({
  auto_version: '2.0.0',
  versions: [{
    version: '2.0.0',
    tag: 'v2.0.0',
    channels: ['auto', 'manual']
  }]
});
delete process.env.NIYIEN_PLUGIN_RELEASE_TAG;
delete process.env.NIYIEN_PLUGINS_DISABLED;
delete process.env.NIYIEN_CONTENT_RELEASE_TAG;
delete process.env.NIYIEN_DATA_RELEASE_TAG;
const req = {
  query: { country: 'CN', platform: 'windows' },
  headers: { host: 'www.niyien.com', 'x-forwarded-proto': 'https' },
  socket: {}
};
const res = {
  setHeader() {},
  status() { return this; },
  json(payload) { this.payload = payload; }
};
handler(req, res).then(() => {
  if (!res.payload.plugins_base.endsWith('/content/v2.0.0/plugins/')) {
    throw new Error(`plugins_base: ${res.payload.plugins_base}`);
  }
  if (res.payload.plugins_base.includes('%2Fplugins')) {
    throw new Error(`encoded slash: ${res.payload.plugins_base}`);
  }
}).catch((err) => {
  console.error(err);
  process.exit(1);
});
"""
        subprocess.run(["node", "-e", script], cwd=REPO_ROOT, check=True)

    def test_hide_version_preserves_unchecked_plugin_on_promoted_auto_version(self):
        api = ReleasePlanApi()
        api.vercel.policy_json = """{
  "auto_version": "2.0.0",
  "versions": [
    {
      "version": "2.0.0",
      "tag": "v2.0.0",
      "channels": ["auto", "manual"],
      "plugin_tag": "plugin-live",
      "plugins_source_mode": "release",
      "plugins_source_ref": "v2.0.0",
      "plugins_source_tag": "v2.0.0",
      "global_plugins_base": "https://example.test/plugins/"
    },
    {
      "version": "1.0.0",
      "tag": "v1.0.0",
      "channels": ["manual"]
    }
  ]
}"""

        result = Api.execute_app_action(
            api,
            {
                "action": "hide_version",
                "source_kind": "release",
                "version": "2.0.0",
                "tag": "v2.0.0",
                "scope": ["app"],
            },
        )

        self.assertTrue(result["ok"])
        policy_json = api.vercel.upserts[0]["NIYIEN_RELEASE_POLICY_JSON"]
        self.assertIn('"auto_version": "1.0.0"', policy_json)
        self.assertIn('"version": "1.0.0"', policy_json)
        self.assertIn('"plugin_tag": "plugin-live"', policy_json)
        self.assertIn('"plugins_source_mode": "release"', policy_json)
        self.assertIn('"plugins_source_ref": "v2.0.0"', policy_json)

    def test_hide_version_preserves_unchecked_plugin_only_on_promoted_auto_entry(self):
        api = ReleasePlanApi()
        api.vercel.envs["NIYIEN_PLUGIN_RELEASE_TAG"] = "plugin-current-env"
        api.vercel.policy_json = """{
  "auto_version": "2.0.0",
  "versions": [
    {
      "version": "2.0.0",
      "tag": "v2.0.0",
      "channels": ["auto", "manual"],
      "plugin_tag": "plugin-hidden-stale",
      "plugins_source_mode": "release",
      "plugins_source_ref": "v2.0.0",
      "plugins_source_tag": "v2.0.0"
    },
    {
      "version": "1.0.0",
      "tag": "v1.0.0",
      "channels": ["manual"]
    },
    {
      "version": "0.9.0",
      "tag": "v0.9.0",
      "channels": ["manual"],
      "plugin_tag": "plugin-manual"
    }
  ]
}"""

        result = Api.execute_app_action(
            api,
            {
                "action": "hide_version",
                "source_kind": "release",
                "version": "2.0.0",
                "tag": "v2.0.0",
                "scope": ["app"],
            },
        )

        self.assertTrue(result["ok"])
        policy = json.loads(api.vercel.upserts[0]["NIYIEN_RELEASE_POLICY_JSON"])
        promoted = next(v for v in policy["versions"] if v["version"] == "1.0.0")
        manual = next(v for v in policy["versions"] if v["version"] == "0.9.0")
        self.assertEqual(promoted["plugin_tag"], "plugin-current-env")
        self.assertNotIn("plugin-hidden-stale", api.vercel.upserts[0]["NIYIEN_RELEASE_POLICY_JSON"])
        self.assertEqual(manual["plugin_tag"], "plugin-manual")

    def test_hide_version_clears_checked_plugin_resources(self):
        api = ReleasePlanApi()
        api.vercel.policy_json = """{
  "auto_version": "2.0.0",
  "versions": [
    {
      "version": "2.0.0",
      "tag": "v2.0.0",
      "channels": ["auto", "manual"],
      "plugin_tag": "plugin-live",
      "plugins_source_mode": "release",
      "plugins_source_ref": "v2.0.0",
      "plugins_source_tag": "v2.0.0"
    },
    {
      "version": "1.0.0",
      "tag": "v1.0.0",
      "channels": ["manual"],
      "plugin_tag": "plugin-live",
      "plugins_source_mode": "release",
      "plugins_source_ref": "v2.0.0",
      "plugins_source_tag": "v2.0.0"
    }
  ]
}"""

        result = Api.execute_app_action(
            api,
            {
                "action": "hide_version",
                "source_kind": "release",
                "version": "2.0.0",
                "tag": "v2.0.0",
                "scope": ["app", "plugin"],
            },
        )

        self.assertTrue(result["ok"])
        upsert = api.vercel.upserts[0]
        self.assertEqual(upsert["NIYIEN_PLUGIN_RELEASE_TAG"], "")
        self.assertEqual(upsert["NIYIEN_PLUGINS_DISABLED"], "1")
        self.assertEqual(upsert["NIYIEN_PLUGINS_SOURCE_MODE"], "")
        self.assertEqual(upsert["NIYIEN_PLUGINS_TAG"], "")
        policy_json = upsert["NIYIEN_RELEASE_POLICY_JSON"]
        self.assertIn('"auto_version": "1.0.0"', policy_json)
        self.assertNotIn('"plugin_tag": "plugin-live"', policy_json)
        self.assertNotIn('"plugins_source_ref": "v2.0.0"', policy_json)

    def test_hide_version_clears_checked_lens_and_sdk_resources(self):
        api = ReleasePlanApi()

        result = Api.execute_app_action(
            api,
            {
                "action": "hide_version",
                "source_kind": "release",
                "version": "1.0.0",
                "tag": "v1.0.0",
                "scope": ["app", "lens", "sdk"],
            },
        )

        self.assertTrue(result["ok"])
        upsert = api.vercel.upserts[0]
        self.assertEqual(upsert["NIYIEN_LENS_DISABLED"], "1")
        self.assertEqual(upsert["NIYIEN_SDK_DISABLED"], "1")
        self.assertEqual(upsert["NIYIEN_LENS_DATA_TAG"], "")
        self.assertEqual(upsert["NIYIEN_LENS_RELEASE_TAG"], "")
        self.assertEqual(upsert["NIYIEN_SDK_BASE"], "")

    def test_release_plan_switch_auto_preserves_recommended_when_not_explicitly_set(self):
        api = ReleasePlanApi()
        api.vercel.policy_json = """{
  "auto_version": "1.0.0",
  "versions": [
    {
      "version": "1.0.0",
      "tag": "v1.0.0",
      "channels": ["manual"],
      "recommended": true
    }
  ]
}"""

        result = api.execute_release_plan(
            {
                "action": "switch_auto",
                "source_kind": "release",
                "version": "1.0.0",
                "tag": "v1.0.0",
                "resources": {
                    "lens_tag": "",
                    "plugin_mode": "release",
                    "plugin_tag": "",
                },
            }
        )

        self.assertTrue(result["ok"])
        note = api.captured_finalize({})
        self.assertIn("deploy hook skipped", note)
        self.assertEqual(len(api.vercel.upserts), 1)
        policy_json = api.vercel.upserts[0]["NIYIEN_RELEASE_POLICY_JSON"]
        self.assertIn('"recommended": true', policy_json)

    def test_release_plan_rollback_auto_keeps_legacy_recommended_true(self):
        api = ReleasePlanApi()
        api.vercel.policy_json = """{
  "auto_version": "1.0.0",
  "versions": [
    {
      "version": "1.0.0",
      "tag": "v1.0.0",
      "channels": ["manual"],
      "recommended": false
    }
  ]
}"""

        result = api.execute_release_plan(
            {
                "action": "rollback_auto",
                "source_kind": "release",
                "version": "1.0.0",
                "tag": "v1.0.0",
                "resources": {
                    "lens_tag": "",
                    "plugin_mode": "release",
                    "plugin_tag": "",
                },
            }
        )

        self.assertTrue(result["ok"])
        note = api.captured_finalize({})
        self.assertIn("deploy hook skipped", note)
        self.assertEqual(len(api.vercel.upserts), 1)
        policy_json = api.vercel.upserts[0]["NIYIEN_RELEASE_POLICY_JSON"]
        self.assertIn('"recommended": true', policy_json)

    def test_release_plan_does_not_write_manifest_when_pan123_start_fails(self):
        api = ReleasePlanApi({"ok": False, "error": "missing creds"})

        result = api.execute_release_plan(
            {
                "action": "publish_and_push",
                "source_kind": "release",
                "version": "1.2.3",
                "tag": "v1.2.3",
                "resources": {
                    "lens_tag": "data-v20260501.1",
                    "plugin_mode": "release",
                    "plugin_tag": "v1.2.3",
                },
            }
        )

        self.assertFalse(result["ok"])
        self.assertIn("missing creds", result["error"])
        self.assertEqual(api.vercel.upserts, [])


if __name__ == "__main__":
    unittest.main()
