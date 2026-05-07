import json
import unittest
from pathlib import Path

from distribution.control_center.backend import api as api_module
from distribution.control_center.backend.api import Api


FRONTEND_ROOT = Path(__file__).resolve().parents[1] / "frontend"


class ReleaseCenterUiTests(unittest.TestCase):
    def test_resources_are_not_a_separate_top_level_view(self):
        html = (FRONTEND_ROOT / "index.html").read_text(encoding="utf-8")

        self.assertNotIn('data-view="resources"', html)
        self.assertNotIn('data-action-nav="resources"', html)
        self.assertIn('data-view="publish"', html)
        self.assertIn('id="open-manifest-modal-btn"', html)
        self.assertIn('id="manifest-modal"', html)
        self.assertIn('id="plan-include-lens"', html)
        self.assertIn('id="plan-include-plugin"', html)
        self.assertIn('id="plan-include-sdk"', html)

    def test_dashboard_has_no_publish_or_upload_actions(self):
        html = (FRONTEND_ROOT / "index.html").read_text(encoding="utf-8")
        app_js = (FRONTEND_ROOT / "app.js").read_text(encoding="utf-8")
        dashboard_html = html.split('<!-- Publish view', 1)[0]

        self.assertNotIn('data-action-nav="publish"', dashboard_html)
        self.assertNotIn('open-manifest-modal-btn', dashboard_html)
        self.assertNotIn('dash-pan123-manual-upload-btn', app_js)
        self.assertNotIn('dashTriggerManualUpload', app_js)
        self.assertNotIn('resources-publish-all-btn', html)
        self.assertNotIn('resources-execute-btn', html)

    def test_plugin_action_artifact_uses_selectable_build_list(self):
        html = (FRONTEND_ROOT / "index.html").read_text(encoding="utf-8")
        app_js = (FRONTEND_ROOT / "app.js").read_text(encoding="utf-8")

        self.assertIn('id="plan-plugin-artifact-list"', html)
        self.assertIn('id="plan-plugin-artifact-selected"', html)
        self.assertIn('id="plan-plugin-run-id"', html)
        self.assertIn('type="hidden" id="plan-plugin-artifact"', html)
        self.assertNotIn('placeholder="GyroflowNiyien-frei0r-windows 等 CSV"', html)
        self.assertIn("list_plugin_action_builds", app_js)
        self.assertIn("selectPluginArtifactItem", app_js)


# ---- Hidden Management tab (release-hidden-management capability) ----

class FakeHiddenVercel:
    """Vercel double for HiddenManagement tests.

    Stores `policy_dict` as the source of truth so tests can seed and
    assert against a structured value; serializes to JSON only at the
    Vercel boundary like the real client. Tracks all upsert calls so
    tests can assert atomicity (exactly one upsert per submission).
    """

    def __init__(self, policy_dict: dict):
        self._policy = dict(policy_dict)
        self.upserts: list[dict] = []
        self.list_env_records_calls = 0
        self.get_env_value_calls = 0

    def list_env_records(self) -> dict:
        self.list_env_records_calls += 1
        return {"NIYIEN_RELEASE_POLICY_JSON": {"id": "policy-env"}}

    def get_env_value(self, env_id: str) -> str:
        self.get_env_value_calls += 1
        if env_id != "policy-env":
            raise RuntimeError("unexpected env id")
        return json.dumps(self._policy, ensure_ascii=False)

    def list_envs_decrypted(self) -> dict:
        return {"NIYIEN_RELEASE_POLICY_JSON": json.dumps(self._policy, ensure_ascii=False)}

    def upsert_envs(self, mapping: dict) -> dict:
        self.upserts.append(dict(mapping))
        if "NIYIEN_RELEASE_POLICY_JSON" in mapping:
            self._policy = json.loads(mapping["NIYIEN_RELEASE_POLICY_JSON"])
        return {"ok": True}

    def upsert_env(self, name: str, value: str) -> dict:
        return self.upsert_envs({name: value})

    @property
    def current_policy(self) -> dict:
        return dict(self._policy)


class FakeHiddenGithub:
    def list_releases(self) -> list:
        return []


class HiddenManagementApi(Api):
    """Api subclass wired to FakeHiddenVercel/FakeHiddenGithub plus a
    counted deploy hook so tests can assert exactly-one dispatch.
    """

    def __init__(self, policy_dict: dict):
        self.vercel = FakeHiddenVercel(policy_dict)
        self.github = FakeHiddenGithub()
        self.deploy_hook_calls = 0

    def _vercel(self, cfg=None):
        return self.vercel

    def _github(self, cfg=None):
        return self.github

    def _trigger_deploy_hook(self, cfg):
        self.deploy_hook_calls += 1
        return f"deploy hook stub #{self.deploy_hook_calls}"


class HiddenManagementHelperTests(unittest.TestCase):
    def test_canonical_plugin_key_release(self):
        key = Api._canonical_plugin_key({"plugin_tag": "v1.6.3"})
        self.assertEqual(key, {"kind": "release", "ref": "v1.6.3"})

    def test_canonical_plugin_key_release_explicit_mode(self):
        key = Api._canonical_plugin_key({
            "plugins_source_mode": "release",
            "plugin_tag": "v1.6.1",
        })
        self.assertEqual(key, {"kind": "release", "ref": "v1.6.1"})

    def test_canonical_plugin_key_artifact(self):
        key = Api._canonical_plugin_key({
            "plugins_source_mode": "artifact",
            "plugins_source_ref": "actions-run-1234",
        })
        self.assertEqual(key, {"kind": "artifact", "run_id": 1234})

    def test_canonical_plugin_key_none(self):
        self.assertIsNone(Api._canonical_plugin_key({}))
        self.assertIsNone(Api._canonical_plugin_key({"plugin_tag": ""}))
        self.assertIsNone(Api._canonical_plugin_key({
            "plugins_source_mode": "artifact",
            "plugins_source_ref": "garbage",
        }))

    def test_plugin_keys_match(self):
        a = {"kind": "release", "ref": "v1"}
        self.assertTrue(Api._plugin_keys_match(a, {"kind": "release", "ref": "v1"}))
        self.assertFalse(Api._plugin_keys_match(a, {"kind": "release", "ref": "v2"}))
        self.assertFalse(Api._plugin_keys_match(a, {"kind": "artifact", "run_id": 1}))
        self.assertTrue(Api._plugin_keys_match(
            {"kind": "artifact", "run_id": 7},
            {"kind": "artifact", "run_id": 7},
        ))
        # Stringified run_id still matches integer.
        self.assertTrue(Api._plugin_keys_match(
            {"kind": "artifact", "run_id": "7"},
            {"kind": "artifact", "run_id": 7},
        ))

    def test_derive_plugin_inventory_dedupes(self):
        versions = [
            {"version": "1.6.3", "plugin_tag": "v1.6.3"},
            {"version": "1.6.2", "plugin_tag": "v1.6.2"},
            {"version": "1.6.2-beta", "plugin_tag": "v1.6.2"},  # same plugin
            {
                "version": "1.5.0",
                "plugins_source_mode": "artifact",
                "plugins_source_ref": "actions-run-99",
            },
            {"version": "1.4.0"},  # no plugin, skipped
        ]
        inv = Api._derive_plugin_inventory(versions)
        self.assertEqual(len(inv), 3)
        # First plugin is v1.6.3 used by 1.6.3 only.
        self.assertEqual(inv[0]["key"], {"kind": "release", "ref": "v1.6.3"})
        self.assertEqual(inv[0]["used_by_app_versions"], ["1.6.3"])
        # Second plugin is v1.6.2 used by both 1.6.2 and 1.6.2-beta.
        self.assertEqual(inv[1]["key"], {"kind": "release", "ref": "v1.6.2"})
        self.assertEqual(inv[1]["used_by_app_versions"], ["1.6.2", "1.6.2-beta"])
        # Third is artifact run 99.
        self.assertEqual(inv[2]["key"], {"kind": "artifact", "run_id": 99})


class HiddenManagementApiTests(unittest.TestCase):
    def setUp(self):
        self._orig_load_config = api_module.config_module.load_config
        api_module.config_module.load_config = lambda: {
            "github_owner": "NiYien",
            "github_repo": "gyroflow",
            "network_proxy": "",
        }

    def tearDown(self):
        api_module.config_module.load_config = self._orig_load_config

    @staticmethod
    def _seed_policy() -> dict:
        return {
            "auto_version": "1.6.3-ni.5",
            "versions": [
                {
                    "version": "1.6.3-ni.5",
                    "tag": "v1.6.3-niyien.1",
                    "channels": ["auto", "manual"],
                    "recommended": True,
                    "changelog": "首次 code/data 分仓发版",
                    "plugin_tag": "v1.6.3",
                },
                {
                    "version": "1.6.2-ni.4",
                    "tag": "v1.6.2-niyien.1",
                    "channels": ["manual"],
                    "recommended": False,
                    "changelog": "稳定性改进",
                    "plugin_tag": "v1.6.2",
                },
                {
                    "version": "1.6.1-ni.3",
                    "tag": "v1.6.1-niyien.1",
                    "channels": ["manual"],
                    "recommended": False,
                    "changelog": "lens preset 更新",
                    "plugin_tag": "v1.6.1",
                },
            ],
            "hidden_plugins": [
                {"kind": "release", "ref": "v1.6.1"},
            ],
        }

    def test_list_hidden_view_data_basic(self):
        api = HiddenManagementApi(self._seed_policy())
        result = api.list_hidden_view_data()
        self.assertTrue(result["ok"], msg=result)
        self.assertEqual(result["auto_version"], "1.6.3-ni.5")
        self.assertEqual(len(result["app_versions"]), 3)
        # Spot-check first row carries all surfaced fields.
        first = result["app_versions"][0]
        self.assertEqual(first["version"], "1.6.3-ni.5")
        self.assertEqual(first["tag"], "v1.6.3-niyien.1")
        self.assertEqual(first["channels"], ["auto", "manual"])
        self.assertEqual(first["plugin_tag"], "v1.6.3")
        self.assertEqual(first["plugin_run_id"], 0)
        self.assertEqual(first["published_at"], "")  # no GH releases stub
        # Derived plugins: 3 distinct, v1.6.1 marked hidden.
        self.assertEqual(len(result["derived_plugins"]), 3)
        v161 = next(p for p in result["derived_plugins"] if p.get("ref") == "v1.6.1")
        self.assertTrue(v161["hidden"])
        self.assertEqual(v161["used_by_app_versions"], ["1.6.1-ni.3"])
        # No orphans: blacklist entry has a matching version row.
        self.assertEqual(result["extra_hidden_plugins"], [])

    def test_list_hidden_view_data_orphan_plugin(self):
        policy = self._seed_policy()
        policy["hidden_plugins"].append({"kind": "release", "ref": "v0.9.0"})
        policy["hidden_plugins"].append({"kind": "artifact", "run_id": 9999})
        api = HiddenManagementApi(policy)
        result = api.list_hidden_view_data()
        self.assertTrue(result["ok"], msg=result)
        # Two orphans surface in extra_hidden_plugins.
        self.assertEqual(len(result["extra_hidden_plugins"]), 2)
        kinds = sorted(e["kind"] for e in result["extra_hidden_plugins"])
        self.assertEqual(kinds, ["artifact", "release"])
        rel = next(e for e in result["extra_hidden_plugins"] if e["kind"] == "release")
        self.assertEqual(rel["ref"], "v0.9.0")
        self.assertTrue(rel["hidden"])
        art = next(e for e in result["extra_hidden_plugins"] if e["kind"] == "artifact")
        self.assertEqual(art["run_id"], 9999)
        self.assertTrue(art["hidden"])

    def test_apply_hidden_changes_rejects_auto_version(self):
        api = HiddenManagementApi(self._seed_policy())
        result = api.apply_hidden_changes({
            "app_versions_to_hide": ["1.6.3-ni.5"],
            "plugin_keys_to_hide": [],
            "plugin_keys_to_unhide": [],
        })
        self.assertFalse(result["ok"], msg=result)
        self.assertIn("auto_version", result["error"])
        # No side effects.
        self.assertEqual(api.vercel.upserts, [])
        self.assertEqual(api.deploy_hook_calls, 0)

    def test_apply_hidden_changes_batch_atomic(self):
        # Seed an extra entry so we can hide 2 app + 2 plugin keys at once.
        policy = self._seed_policy()
        policy["versions"].append({
            "version": "1.5.9-ni.2",
            "tag": "v1.5.9-niyien.1",
            "channels": ["manual"],
            "recommended": False,
            "changelog": "older",
            "plugins_source_mode": "artifact",
            "plugins_source_ref": "actions-run-1234",
        })
        api = HiddenManagementApi(policy)
        result = api.apply_hidden_changes({
            "app_versions_to_hide": ["1.6.2-ni.4", "1.5.9-ni.2"],
            "plugin_keys_to_hide": [
                {"kind": "release", "ref": "v1.6.2"},
                {"kind": "artifact", "run_id": 1234},
            ],
            "plugin_keys_to_unhide": [],
        })
        self.assertTrue(result["ok"], msg=result)
        # Atomicity: exactly one Vercel upsert + one deploy hook dispatch.
        self.assertEqual(len(api.vercel.upserts), 1)
        self.assertEqual(api.deploy_hook_calls, 1)
        self.assertIn("NIYIEN_RELEASE_POLICY_JSON", api.vercel.upserts[0])
        # Final policy: 2 entries removed, 2 plugin keys appended to blacklist.
        final = api.vercel.current_policy
        version_ids = sorted(v["version"] for v in final["versions"])
        self.assertEqual(version_ids, ["1.6.1-ni.3", "1.6.3-ni.5"])
        # hidden_plugins: original v1.6.1 plus the two new keys.
        self.assertEqual(len(final["hidden_plugins"]), 3)
        kinds_refs = sorted(
            (h["kind"], h.get("ref") or h.get("run_id"))
            for h in final["hidden_plugins"]
        )
        self.assertEqual(
            kinds_refs,
            sorted([
                ("release", "v1.6.1"),
                ("release", "v1.6.2"),
                ("artifact", 1234),
            ]),
        )

    def test_apply_hidden_changes_preserves_entry_plugin_tag(self):
        api = HiddenManagementApi(self._seed_policy())
        result = api.apply_hidden_changes({
            "app_versions_to_hide": [],
            "plugin_keys_to_hide": [{"kind": "release", "ref": "v1.6.1"}],
            "plugin_keys_to_unhide": [],
        })
        # v1.6.1 is already in the seed blacklist; this is a no-op for the
        # blacklist (deduped), but it MUST still leave entry.plugin_tag intact.
        self.assertTrue(result["ok"], msg=result)
        final = api.vercel.current_policy
        v161_entry = next(v for v in final["versions"] if v["version"] == "1.6.1-ni.3")
        self.assertEqual(v161_entry["plugin_tag"], "v1.6.1")
        # And the blacklist did not duplicate.
        self.assertEqual(len(final["hidden_plugins"]), 1)

    def test_apply_hidden_changes_unhide_removes_blacklist_entry(self):
        api = HiddenManagementApi(self._seed_policy())
        result = api.apply_hidden_changes({
            "app_versions_to_hide": [],
            "plugin_keys_to_hide": [],
            "plugin_keys_to_unhide": [{"kind": "release", "ref": "v1.6.1"}],
        })
        self.assertTrue(result["ok"], msg=result)
        final = api.vercel.current_policy
        self.assertEqual(final["hidden_plugins"], [])

    def test_apply_hidden_changes_rejects_malformed_plugin_key(self):
        api = HiddenManagementApi(self._seed_policy())
        result = api.apply_hidden_changes({
            "app_versions_to_hide": [],
            "plugin_keys_to_hide": [{"kind": "weird", "ref": "x"}],
            "plugin_keys_to_unhide": [],
        })
        self.assertFalse(result["ok"], msg=result)
        self.assertIn("kind", result["error"])
        self.assertEqual(api.vercel.upserts, [])
        self.assertEqual(api.deploy_hook_calls, 0)


class UpsertVersionEntryInheritanceTests(unittest.TestCase):
    """Regression: app-only publish must not blank out plugin manifest fields.

    See api.py::_upsert_version_entry docstring — when a new version is
    appended (i.e. the publish carries no plugin scope), the entry must
    inherit plugin/lens/sdk fields from the existing auto entry, otherwise
    the docs manifest Global branch falls back to release-latest base while
    leaving plugins_source_ref/tag empty, which the gyroflow client treats
    as a permanent "plugin update available" false positive.
    """

    @staticmethod
    def _auto_entry_with_plugin_info() -> dict:
        return {
            "version": "1.6.3-ni.28",
            "tag": "run-25441462957",
            "channels": ["auto", "manual"],
            "changelog": "previous build",
            "recommended": True,
            "plugins_source_mode": "artifact",
            "plugins_source_ref": "actions-run-25441462957",
            "plugins_source_tag": "GyroflowNiyien-Adobe-windows, GyroflowNiyien-OpenFX-windows",
            "plugin_tag": "plugin-0549a827ac4f",
            "global_plugins_base": "https://nightly.link/NiYien/gyroflow-plugins/actions/runs/25441462957/",
            "lens_release_tag": "data-v20260501.1",
            "lens_tag": "lens-f0061df6d394",
            "lens_version": 7,
            "lens_sha256": "23246da574a05429e76bcc01f4760ad601757214988084055f675079e77a8f8d",
        }

    def test_app_only_append_inherits_plugin_fields_from_auto_entry(self):
        versions = [self._auto_entry_with_plugin_info()]
        Api._upsert_version_entry(
            versions,
            version="1.6.3-ni.29",
            tag="run-25475562063",
            changelog="change version display",
            recommended=True,
            channels=["auto", "manual"],
            run_id=25475562063,
            app_source_mode="artifact",
            app_urls={"windows": {"installer_url": "https://nightly.link/.../setup.zip"}},
        )
        new_entry = next(v for v in versions if v["version"] == "1.6.3-ni.29")
        # Inherited plugin / lens fields preserve manifest plugin source identity.
        self.assertEqual(new_entry["plugins_source_mode"], "artifact")
        self.assertEqual(new_entry["plugins_source_ref"], "actions-run-25441462957")
        self.assertIn("GyroflowNiyien-Adobe-windows", new_entry["plugins_source_tag"])
        self.assertEqual(new_entry["plugin_tag"], "plugin-0549a827ac4f")
        self.assertEqual(
            new_entry["global_plugins_base"],
            "https://nightly.link/NiYien/gyroflow-plugins/actions/runs/25441462957/",
        )
        self.assertEqual(new_entry["lens_release_tag"], "data-v20260501.1")
        self.assertEqual(new_entry["lens_version"], 7)
        # App-specific fields written by the publish itself.
        self.assertEqual(new_entry["tag"], "run-25475562063")
        self.assertEqual(new_entry["run_id"], 25475562063)
        self.assertEqual(new_entry["app_source_mode"], "artifact")

    def test_append_falls_back_to_any_donor_when_no_auto_entry(self):
        donor = self._auto_entry_with_plugin_info()
        donor["channels"] = ["manual"]  # no auto entry exists
        versions = [donor]
        Api._upsert_version_entry(
            versions,
            version="1.6.3-ni.29",
            tag="run-25475562063",
            changelog="cl",
            recommended=False,
            channels=["manual"],
        )
        new_entry = next(v for v in versions if v["version"] == "1.6.3-ni.29")
        # Falls back to the only entry that carries plugin info.
        self.assertEqual(new_entry["plugins_source_ref"], "actions-run-25441462957")
        self.assertEqual(new_entry["plugin_tag"], "plugin-0549a827ac4f")

    def test_append_with_no_donor_leaves_plugin_fields_unset(self):
        # First-ever publish: nothing to inherit, entry should be plain.
        versions: list[dict] = []
        Api._upsert_version_entry(
            versions, version="1.0.0", tag="v1.0.0", changelog="initial",
            recommended=True, channels=["auto", "manual"],
        )
        self.assertEqual(len(versions), 1)
        for k in Api._RESOURCE_INHERIT_KEYS:
            self.assertNotIn(k, versions[0])

    def test_merge_branch_still_preserves_existing_plugin_fields(self):
        # Republishing an existing version must not regress the merge path:
        # unknown / plugin fields on the existing entry survive.
        versions = [self._auto_entry_with_plugin_info()]
        Api._upsert_version_entry(
            versions,
            version="1.6.3-ni.28",  # same as existing
            tag="run-25441462957",
            changelog="updated changelog",
            recommended=False,
            channels=["manual"],
        )
        merged = next(v for v in versions if v["version"] == "1.6.3-ni.28")
        self.assertEqual(merged["plugins_source_ref"], "actions-run-25441462957")
        self.assertEqual(merged["plugin_tag"], "plugin-0549a827ac4f")
        self.assertEqual(merged["changelog"], "updated changelog")
        self.assertEqual(merged["channels"], ["manual"])


if __name__ == "__main__":
    unittest.main()
