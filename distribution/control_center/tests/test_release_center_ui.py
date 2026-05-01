import unittest
from pathlib import Path


FRONTEND_ROOT = Path(__file__).resolve().parents[1] / "frontend"


class ReleaseCenterUiTests(unittest.TestCase):
    def test_resources_are_not_a_separate_top_level_view(self):
        html = (FRONTEND_ROOT / "index.html").read_text(encoding="utf-8")

        self.assertNotIn('data-view="resources"', html)
        self.assertNotIn('data-action-nav="resources"', html)
        self.assertIn('data-view="publish"', html)
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


if __name__ == "__main__":
    unittest.main()
