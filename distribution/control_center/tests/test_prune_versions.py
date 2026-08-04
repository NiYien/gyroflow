"""Tests for Api._prune_policy_versions — caps policy.versions[] to the N
most-recent entries so NIYIEN_RELEASE_POLICY_JSON stays under Vercel's 64KB
env-var ceiling, while always retaining the in-service auto_version.

Pruning keeps the LEADING `keep` entries and therefore inherits whatever
ordering the caller applied. That ordering must be `version_sort_key` (the
mirror of the client's `compare_app_versions`); the string ordering it
replaced silently pruned any version whose sequence had more digits than
its peers.
"""

import unittest

from distribution.control_center.backend.api import Api
from distribution.control_center.backend.changelog_archive import version_sort_key


def _versions(*nums):
    # Newest-first, matching the sort callers apply before serializing.
    return [{"version": f"1.6.3-ni.{n}", "tag": f"v1.6.3-ni.{n}"} for n in nums]


def _entries(*version_strings):
    return [{"version": v, "tag": f"v{v}"} for v in version_strings]


def _sort_and_prune(policy, keep=10):
    """Replay what every publish action does: sort newest-first, then cap.

    Mirrors api.py's `_execute_release_plan` / `execute_app_action` /
    `apply_hidden_changes`, which all sort with `version_sort_key` right
    before calling `_prune_policy_versions`.
    """
    policy["versions"].sort(key=lambda x: version_sort_key(x.get("version", "")), reverse=True)
    return Api._prune_policy_versions(policy, keep=keep)


class PrunePolicyVersionsTests(unittest.TestCase):
    def test_no_prune_when_under_cap(self):
        policy = {"versions": _versions(10, 9, 8), "auto_version": "1.6.3-ni.10"}
        removed = Api._prune_policy_versions(policy, keep=10)
        self.assertEqual(removed, 0)
        self.assertEqual(len(policy["versions"]), 3)

    def test_no_prune_when_exactly_cap(self):
        nums = list(range(20, 10, -1))  # 10 entries, newest-first
        policy = {"versions": _versions(*nums), "auto_version": "1.6.3-ni.20"}
        removed = Api._prune_policy_versions(policy, keep=10)
        self.assertEqual(removed, 0)
        self.assertEqual(len(policy["versions"]), 10)

    def test_keeps_most_recent_when_over_cap(self):
        nums = list(range(18, 0, -1))  # 18 entries ni.18..ni.1, newest-first
        policy = {"versions": _versions(*nums), "auto_version": "1.6.3-ni.18"}
        removed = Api._prune_policy_versions(policy, keep=10)
        self.assertEqual(removed, 8)
        kept = [v["version"] for v in policy["versions"]]
        # The 10 newest survive; ni.8..ni.1 dropped.
        self.assertEqual(kept, [f"1.6.3-ni.{n}" for n in range(18, 8, -1)])

    def test_auto_version_inside_window_no_graft(self):
        nums = list(range(18, 0, -1))
        policy = {"versions": _versions(*nums), "auto_version": "1.6.3-ni.15"}
        Api._prune_policy_versions(policy, keep=10)
        kept = [v["version"] for v in policy["versions"]]
        self.assertIn("1.6.3-ni.15", kept)
        self.assertEqual(len(kept), 10)

    def test_old_auto_version_is_retained(self):
        # auto_version is an OLD build (ni.2) outside the recent-10 window;
        # it MUST survive — dropping it would orphan the served manifest.
        nums = list(range(18, 0, -1))
        policy = {"versions": _versions(*nums), "auto_version": "1.6.3-ni.2"}
        removed = Api._prune_policy_versions(policy, keep=10)
        kept = [v["version"] for v in policy["versions"]]
        self.assertIn("1.6.3-ni.2", kept)
        # Cap still honored: exactly 10 entries.
        self.assertEqual(len(kept), 10)
        self.assertEqual(removed, 8)
        # The oldest of the recent window (ni.9) was dropped to make room.
        self.assertNotIn("1.6.3-ni.9", kept)
        # Result stays newest-first sorted — by sequence number, not by the
        # string form (which would rank ni.2 above ni.18).
        self.assertEqual(kept, sorted(kept, key=version_sort_key, reverse=True))

    def test_missing_versions_key_is_safe(self):
        policy = {"auto_version": "x"}
        self.assertEqual(Api._prune_policy_versions(policy, keep=10), 0)

    def test_non_list_versions_is_safe(self):
        policy = {"versions": "garbage", "auto_version": "x"}
        self.assertEqual(Api._prune_policy_versions(policy, keep=10), 0)

    def test_empty_auto_version_just_truncates(self):
        nums = list(range(18, 0, -1))
        policy = {"versions": _versions(*nums), "auto_version": ""}
        removed = Api._prune_policy_versions(policy, keep=10)
        self.assertEqual(removed, 8)
        self.assertEqual(len(policy["versions"]), 10)

    def test_keep_less_than_one_is_noop(self):
        policy = {"versions": _versions(3, 2, 1), "auto_version": "1.6.3-ni.3"}
        self.assertEqual(Api._prune_policy_versions(policy, keep=0), 0)
        self.assertEqual(len(policy["versions"]), 3)


class SortThenPruneOrderingTests(unittest.TestCase):
    """Regressions for the ordering the prune step depends on.

    Every case here passed the string comparator only by coincidence of equal
    digit counts; each one breaks the moment a component grows a digit.
    """

    def test_three_digit_sequence_is_not_pruned_as_oldest(self):
        # Real incident (2026-08-04): ni.100 published into the manual list
        # never reached the manifest. As a string "1.6.3-ni.100" sorts below
        # "1.6.3-ni.81" ('1' < '8'), so it landed at index 10 of 11 and the
        # keep=10 cap deleted it — while the publish reported success.
        policy = {
            "versions": _versions(100, 98, 95, 94, 93, 92, 90, 87, 85, 82, 81),
            "auto_version": "1.6.3-ni.98",
        }
        removed = _sort_and_prune(policy, keep=10)
        kept = [v["version"] for v in policy["versions"]]
        self.assertEqual(removed, 1)
        self.assertIn("1.6.3-ni.100", kept)
        self.assertEqual(kept[0], "1.6.3-ni.100")
        # The genuinely oldest entry is the one that goes.
        self.assertNotIn("1.6.3-ni.81", kept)

    def test_bare_base_outranks_older_suffixed_build(self):
        # Tag-triggered release builds carry no `-ni.N` suffix at all
        # (build.rs uses the tag verbatim, and release.yml rejects suffixed
        # tags), so bare and suffixed versions coexist in policy.versions.
        # Base comparison wins across bases regardless of any suffix.
        policy = {
            "versions": _entries("1.6.3-ni.100", "1.6.4", "1.6.3-ni.98"),
            "auto_version": "1.6.4",
        }
        _sort_and_prune(policy, keep=10)
        kept = [v["version"] for v in policy["versions"]]
        self.assertEqual(kept, ["1.6.4", "1.6.3-ni.100", "1.6.3-ni.98"])

    def test_bare_base_is_oldest_within_its_own_base(self):
        # Same base: the bare version is that base's first release, so any
        # suffixed build of the same base is newer.
        policy = {
            "versions": _entries("1.6.4", "1.6.4-ni.101"),
            "auto_version": "1.6.4-ni.101",
        }
        _sort_and_prune(policy, keep=10)
        kept = [v["version"] for v in policy["versions"]]
        self.assertEqual(kept, ["1.6.4-ni.101", "1.6.4"])

    def test_multi_digit_base_segment_orders_numerically(self):
        # The same digit-count trap applies to the base triple, not just the
        # sequence: "1.6.10" sorts below "1.6.9" as a string.
        policy = {
            "versions": _entries("1.6.9-ni.200", "1.6.10-ni.201"),
            "auto_version": "1.6.10-ni.201",
        }
        _sort_and_prune(policy, keep=10)
        kept = [v["version"] for v in policy["versions"]]
        self.assertEqual(kept, ["1.6.10-ni.201", "1.6.9-ni.200"])

    def test_retained_old_auto_version_keeps_numeric_position(self):
        # The auto_version graft path re-sorts inside _prune_policy_versions;
        # it must use the same comparator or the grafted entry surfaces at
        # index 0 and hide_version would then promote it as "highest".
        policy = {
            "versions": _versions(100, 98, 95, 94, 93, 92, 90, 87, 85, 82, 81),
            "auto_version": "1.6.3-ni.81",
        }
        _sort_and_prune(policy, keep=10)
        kept = [v["version"] for v in policy["versions"]]
        self.assertEqual(kept[0], "1.6.3-ni.100")
        self.assertEqual(kept[-1], "1.6.3-ni.81")
        self.assertEqual(len(kept), 10)


if __name__ == "__main__":
    unittest.main()
