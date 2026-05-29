"""Tests for Api._prune_policy_versions — caps policy.versions[] to the N
most-recent entries so NIYIEN_RELEASE_POLICY_JSON stays under Vercel's 64KB
env-var ceiling, while always retaining the in-service auto_version.
"""

import unittest

from distribution.control_center.backend.api import Api


def _versions(*nums):
    # Newest-first, matching the sort callers apply before serializing.
    return [{"version": f"1.6.3-ni.{n}", "tag": f"v1.6.3-ni.{n}"} for n in nums]


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
        # Result stays newest-first sorted.
        self.assertEqual(kept, sorted(kept, key=lambda s: s, reverse=True))

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


if __name__ == "__main__":
    unittest.main()
