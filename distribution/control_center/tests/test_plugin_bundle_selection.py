import unittest

from distribution.control_center.backend import api as api_module
from distribution.control_center.backend.api import Api


class FakeVercel:
    def __init__(self):
        self.envs = {
            "NIYIEN_LENS_RELEASE_TAG": "lens-current",
            "NIYIEN_PLUGIN_RELEASE_TAG": "plugin-old",
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

    def _vercel(self, cfg):
        return self.vercel

    def _gh_for(self, owner, repo, cfg):
        return FakeGithub()

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


class PluginBundleSelectionTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
