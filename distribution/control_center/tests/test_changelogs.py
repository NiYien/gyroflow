"""Tests for multi-language changelogs in policy.versions entries.

Covers:
- `Api._normalize_changelogs_input` payload coercion
- `Api._upsert_version_entry` writes both legacy `changelog` and new
  `changelogs` map, omitting empty languages
- `Api._upsert_version_entry` preserves an existing `changelogs` map when
  the caller doesn't pass one (merge path)
- `Api.translate_changelog` wraps translate.py exceptions into structured
  `kind` discriminators for the frontend
"""

import unittest
from unittest.mock import patch

from distribution.control_center.backend.api import Api
from distribution.control_center.backend import translate as translate_module


class NormalizeChangelogsInputTests(unittest.TestCase):
    def test_none_returns_empty(self):
        self.assertEqual(Api._normalize_changelogs_input(None), {})

    def test_non_dict_returns_empty(self):
        self.assertEqual(Api._normalize_changelogs_input("foo"), {})
        self.assertEqual(Api._normalize_changelogs_input(["zh", "en"]), {})

    def test_keeps_non_empty_entries(self):
        out = Api._normalize_changelogs_input({
            "zh": "中文",
            "en": "English",
            "ja": "",         # dropped (empty)
            "ko": "   ",      # dropped (whitespace-only)
            "de": "Deutsch",
        })
        self.assertEqual(out, {"zh": "中文", "en": "English", "de": "Deutsch"})

    def test_drops_non_string_values(self):
        out = Api._normalize_changelogs_input({
            "zh": "中文",
            "en": 42,
            "ja": None,
        })
        self.assertEqual(out, {"zh": "中文"})

    def test_drops_non_string_keys(self):
        out = Api._normalize_changelogs_input({
            "zh": "中文",
            123: "garbage",
        })
        self.assertEqual(out, {"zh": "中文"})

    def test_preserves_internal_whitespace(self):
        # Leading/trailing whitespace is preserved inside the value because
        # Markdown indentation matters; only fully-empty strips are dropped.
        out = Api._normalize_changelogs_input({"zh": "  - 修复 bug\n"})
        self.assertEqual(out, {"zh": "  - 修复 bug\n"})


class UpsertVersionEntryTests(unittest.TestCase):
    def test_writes_both_legacy_and_map(self):
        versions: list[dict] = []
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "中文", True, ["manual"],
            changelogs={"zh": "中文", "en": "English", "ja": "日本語"},
        )
        self.assertEqual(len(versions), 1)
        entry = versions[0]
        self.assertEqual(entry["changelog"], "中文")
        self.assertEqual(
            entry["changelogs"],
            {"zh": "中文", "en": "English", "ja": "日本語"},
        )

    def test_omits_empty_languages_from_map(self):
        versions: list[dict] = []
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "中文", True, ["manual"],
            changelogs={"zh": "中文", "en": "  ", "ja": "日本語"},
        )
        entry = versions[0]
        # `en` was whitespace-only and SHOULD be dropped.
        self.assertEqual(set(entry["changelogs"].keys()), {"zh", "ja"})

    def test_zh_only_payload(self):
        versions: list[dict] = []
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "", True, ["manual"],
            changelogs={"zh": "仅中文"},
        )
        entry = versions[0]
        # Legacy `changelog` SHALL be filled from `zh` when caller passed
        # empty changelog and en is not present in the map either.
        self.assertEqual(entry["changelog"], "仅中文")
        self.assertEqual(entry["changelogs"], {"zh": "仅中文"})

    def test_legacy_fallback_prefers_en_over_zh(self):
        # When the caller doesn't supply a legacy `changelog` explicitly
        # but the changelogs map has both en and zh, the legacy field
        # SHALL be filled from en (better default for international old
        # clients that only read `changelog`).
        versions: list[dict] = []
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "", True, ["manual"],
            changelogs={"zh": "中文", "en": "English", "ja": "日本語"},
        )
        entry = versions[0]
        self.assertEqual(entry["changelog"], "English")
        # The map itself still carries all three.
        self.assertEqual(set(entry["changelogs"].keys()), {"zh", "en", "ja"})

    def test_legacy_fallback_uses_zh_when_en_missing(self):
        # When en isn't translated, zh tracks the final fallback.
        versions: list[dict] = []
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "", True, ["manual"],
            changelogs={"zh": "中文", "ja": "日本語"},
        )
        entry = versions[0]
        self.assertEqual(entry["changelog"], "中文")

    def test_explicit_legacy_changelog_overrides_fallback(self):
        # If the caller (e.g. frontend that picked legacy = en explicitly)
        # passes a non-empty changelog, the fallback chain is skipped.
        versions: list[dict] = []
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "explicit legacy", True, ["manual"],
            changelogs={"zh": "中文", "en": "English"},
        )
        entry = versions[0]
        self.assertEqual(entry["changelog"], "explicit legacy")

    def test_legacy_only_payload(self):
        versions: list[dict] = []
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "legacy text", True, ["manual"],
            changelogs=None,
        )
        entry = versions[0]
        self.assertEqual(entry["changelog"], "legacy text")
        # No changelogs key when none was provided.
        self.assertNotIn("changelogs", entry)

    def test_empty_changelogs_does_not_emit_map(self):
        versions: list[dict] = []
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "中文", True, ["manual"],
            changelogs={},
        )
        entry = versions[0]
        self.assertNotIn("changelogs", entry)

    def test_merge_preserves_legacy_changelog_when_caller_passes_empty(self):
        # Regression for review finding #1: a caller that does NOT carry
        # release notes (empty changelog + empty/None changelogs map) must
        # NOT clobber a previously published entry's legacy changelog.
        # This protects future actions / CLI tools / external scripts that
        # touch an entry without touching its release notes.
        versions: list[dict] = [{
            "version": "1.6.4",
            "tag": "v1.6.4-niyien.1",
            "channels": ["manual"],
            "changelog": "existing legacy text",
            "recommended": False,
        }]
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "", True, ["auto", "manual"],
            changelogs=None,
        )
        entry = versions[0]
        self.assertEqual(entry["changelog"], "existing legacy text")
        # Channels still updated, recommended still flipped — only the
        # changelog string is preserved.
        self.assertEqual(entry["channels"], ["auto", "manual"])
        self.assertTrue(entry["recommended"])

    def test_merge_preserves_existing_changelogs_when_omitted(self):
        # Existing policy entry with an i18n map; new upsert without
        # `changelogs` should keep the map intact (merge path).
        versions: list[dict] = [{
            "version": "1.6.4",
            "tag": "v1.6.4-niyien.1",
            "channels": ["manual"],
            "changelog": "old zh",
            "changelogs": {"zh": "old zh", "en": "old en"},
            "recommended": False,
        }]
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "new zh", True, ["auto", "manual"],
            changelogs=None,
        )
        entry = versions[0]
        # Legacy changelog overwritten with new value.
        self.assertEqual(entry["changelog"], "new zh")
        # Existing map preserved because caller didn't supply one.
        self.assertEqual(
            entry["changelogs"],
            {"zh": "old zh", "en": "old en"},
        )
        # Channels updated.
        self.assertEqual(entry["channels"], ["auto", "manual"])

    def test_merge_replaces_changelogs_when_provided(self):
        versions: list[dict] = [{
            "version": "1.6.4",
            "tag": "v1.6.4-niyien.1",
            "channels": ["manual"],
            "changelog": "old zh",
            "changelogs": {"zh": "old zh", "en": "old en"},
            "recommended": False,
        }]
        Api._upsert_version_entry(
            versions, "1.6.4", "v1.6.4-niyien.1", "new zh", True, ["manual"],
            changelogs={"zh": "new zh", "ja": "new ja"},
        )
        entry = versions[0]
        # New map replaces old, including dropping `en`.
        self.assertEqual(entry["changelogs"], {"zh": "new zh", "ja": "new ja"})

    def test_legacy_entry_load_without_changelogs_does_not_crash(self):
        # A pre-existing policy entry with no `changelogs` field should be
        # readable without error; a fresh upsert can add the map.
        versions: list[dict] = [{
            "version": "1.6.3",
            "tag": "v1.6.3-niyien.1",
            "channels": ["manual"],
            "changelog": "legacy single string",
            "recommended": False,
        }]
        Api._upsert_version_entry(
            versions, "1.6.3", "v1.6.3-niyien.1", "legacy single string", True,
            ["manual"],
            changelogs={"zh": "legacy single string", "en": "Legacy English"},
        )
        entry = versions[0]
        self.assertEqual(
            entry["changelogs"],
            {"zh": "legacy single string", "en": "Legacy English"},
        )


class TranslateChangelogEndpointTests(unittest.TestCase):
    """Verify the `translate_changelog` Api method wraps translate.py
    output and exception types into the structured response the frontend
    expects.
    """

    def _api(self) -> Api:
        # Plain Api() — only translate_changelog is being tested, which
        # uses config_module + translate_module and does not touch vercel
        # or github.
        return Api()

    def test_success_returns_changelogs_map(self):
        canned = {code: f"trans-{code}" for code in translate_module.TARGET_LANGS}
        with patch.object(translate_module, "translate_to_all", return_value=canned):
            with patch(
                "distribution.control_center.backend.api.config_module.load_config",
                return_value={"translate_api_base": "x", "translate_api_key": "y", "translate_model": "z"},
            ):
                resp = self._api().translate_changelog({"zh": "中文"})
        self.assertTrue(resp["ok"])
        self.assertEqual(set(resp["changelogs"].keys()), set(translate_module.TARGET_LANGS))

    def test_config_error_kind(self):
        def raise_config(*a, **kw):
            raise translate_module.TranslateConfigError("missing")
        with patch.object(translate_module, "translate_to_all", side_effect=raise_config):
            with patch(
                "distribution.control_center.backend.api.config_module.load_config",
                return_value={},
            ):
                resp = self._api().translate_changelog({"zh": "中文"})
        self.assertFalse(resp["ok"])
        self.assertEqual(resp["kind"], "config")

    def test_http_error_kind(self):
        def raise_http(*a, **kw):
            raise translate_module.TranslateHTTPError("HTTP 500")
        with patch.object(translate_module, "translate_to_all", side_effect=raise_http):
            with patch(
                "distribution.control_center.backend.api.config_module.load_config",
                return_value={"translate_api_base": "x", "translate_api_key": "y", "translate_model": "z"},
            ):
                resp = self._api().translate_changelog({"zh": "中文"})
        self.assertFalse(resp["ok"])
        self.assertEqual(resp["kind"], "http")

    def test_parse_error_kind(self):
        def raise_parse(*a, **kw):
            raise translate_module.TranslateParseError("bad json")
        with patch.object(translate_module, "translate_to_all", side_effect=raise_parse):
            with patch(
                "distribution.control_center.backend.api.config_module.load_config",
                return_value={"translate_api_base": "x", "translate_api_key": "y", "translate_model": "z"},
            ):
                resp = self._api().translate_changelog({"zh": "中文"})
        self.assertFalse(resp["ok"])
        self.assertEqual(resp["kind"], "parse")


if __name__ == "__main__":
    unittest.main()
