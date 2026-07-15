"""Tests for the docs-repo changelog archive
(changelog-history-page-and-cumulative-notes).

Covers the pure merge/upsert helpers in backend/changelog_archive.py and
the GET-merge-PUT sync flow against a fake GitHub client:

- version_sort_key mirrors the client's compare_app_versions ordering
- upsert overwrites content fields but keeps a non-empty published_at
- self-heal backfill adds policy versions missing from the archive and
  never overwrites existing entries
- sync_archive creates the file on first write (schema header), retries
  once on a stale-sha conflict, and is a no-op when nothing changed
  (migration idempotency)
- Api._sync_changelog_archive_after_publish degrades any failure to a
  warning string (publishing must never be blocked)
"""

import base64
import json
import unittest

from distribution.control_center.backend import changelog_archive as archive
from distribution.control_center.backend.github import ContentsConflictError


def _entry(version, **kw):
    e = {
        "version": version,
        "tag": kw.get("tag", f"v{version}"),
        "published_at": kw.get("published_at", ""),
        "changelog": kw.get("changelog", f"notes {version}"),
        "changelogs": kw.get("changelogs", {}),
        "recommended": kw.get("recommended", False),
    }
    return e


class VersionSortKeyTests(unittest.TestCase):
    def test_ordering_matches_client_rules(self):
        # Mirrors src/distribution.rs::cmp_niyien expectations.
        ordered = [
            "not-a-version",     # unparseable sorts oldest
            "1.6.3",             # bare base is the first release of a base
            "1.6.3-dev.42",      # dev < ni at the same base
            "1.6.3-ni.27",
            "1.6.3-ni.28",       # sequence numeric
            "1.6.4",             # cross-base numeric beats any suffix
        ]
        self.assertEqual(
            sorted(ordered, key=archive.version_sort_key), ordered
        )

    def test_v_prefix_ignored(self):
        self.assertEqual(
            archive.version_sort_key("v1.6.3"), archive.version_sort_key("1.6.3")
        )


class MergeArchiveTests(unittest.TestCase):
    NOW = "2026-07-15T00:00:00Z"

    def test_upsert_new_entry_gets_now_timestamp(self):
        doc = archive.merge_archive(
            archive.empty_archive(),
            upsert_entries=[_entry("1.7.0-ni.2")],
            now_iso=self.NOW,
        )
        self.assertEqual(doc["schema"], archive.ARCHIVE_SCHEMA)
        self.assertEqual(doc["product"], archive.ARCHIVE_PRODUCT)
        self.assertEqual(len(doc["entries"]), 1)
        self.assertEqual(doc["entries"][0]["published_at"], self.NOW)

    def test_upsert_existing_overwrites_content_keeps_published_at(self):
        base = archive.merge_archive(
            archive.empty_archive(),
            upsert_entries=[_entry("1.7.0-ni.2", changelog="old text")],
            now_iso="2026-01-01T00:00:00Z",
        )
        doc = archive.merge_archive(
            base,
            upsert_entries=[_entry("1.7.0-ni.2", changelog="new text", recommended=True)],
            now_iso=self.NOW,
        )
        self.assertEqual(len(doc["entries"]), 1)
        entry = doc["entries"][0]
        self.assertEqual(entry["changelog"], "new text")
        self.assertTrue(entry["recommended"])
        self.assertEqual(entry["published_at"], "2026-01-01T00:00:00Z")

    def test_backfill_only_adds_missing_and_never_overwrites(self):
        base = archive.merge_archive(
            archive.empty_archive(),
            upsert_entries=[_entry("1.6.8-ni.3", changelog="archived text")],
            now_iso="2026-01-01T00:00:00Z",
        )
        doc = archive.merge_archive(
            base,
            backfill_entries=[
                _entry("1.6.8-ni.3", changelog="policy text"),
                _entry("1.6.9-ni.1", changelog="missing one"),
            ],
            now_iso=self.NOW,
        )
        by_version = {e["version"]: e for e in doc["entries"]}
        self.assertEqual(by_version["1.6.8-ni.3"]["changelog"], "archived text")
        self.assertEqual(by_version["1.6.9-ni.1"]["changelog"], "missing one")
        self.assertEqual(by_version["1.6.9-ni.1"]["published_at"], "")

    def test_entries_sorted_descending(self):
        doc = archive.merge_archive(
            archive.empty_archive(),
            upsert_entries=[
                _entry("1.6.3"),
                _entry("1.6.4"),
                _entry("1.6.3-ni.5"),
            ],
            now_iso=self.NOW,
        )
        self.assertEqual(
            [e["version"] for e in doc["entries"]],
            ["1.6.4", "1.6.3-ni.5", "1.6.3"],
        )


class FakeGitHubClient:
    """In-memory Contents API double. `conflicts` makes the first N PUTs
    raise ContentsConflictError (stale sha) to exercise the retry path."""

    def __init__(self, initial: bytes | None = None, conflicts: int = 0):
        self.file = initial
        self.sha = "sha-0" if initial is not None else ""
        self.conflicts = conflicts
        self.put_calls = 0
        self.commit_messages = []

    def get_contents(self, owner=None, repo=None, *, path, ref=""):
        if self.file is None:
            return None
        return {
            "content": base64.b64encode(self.file).decode("ascii"),
            "sha": self.sha,
        }

    def put_contents(self, owner=None, repo=None, *, path, message, content_bytes,
                     sha="", branch=""):
        self.put_calls += 1
        if self.conflicts > 0:
            self.conflicts -= 1
            raise ContentsConflictError("stale sha")
        self.file = content_bytes
        self.sha = f"sha-{self.put_calls}"
        self.commit_messages.append(message)
        return {"content": {"sha": self.sha}}


def _policy_versions():
    return [
        {"version": "1.7.0-ni.2", "tag": "v1.7.0-ni.2", "changelog": "seven",
         "changelogs": {"zh": "七"}, "recommended": True, "channels": ["auto", "manual"]},
        {"version": "1.6.9-ni.1", "tag": "v1.6.9-ni.1", "changelog": "six-nine",
         "channels": ["manual"]},
    ]


class SyncArchiveTests(unittest.TestCase):
    def _sync(self, client, upserts=None):
        return archive.sync_archive(
            client,
            owner="NiYien", repo="docs", branch="main", path="changelog/archive.json",
            commit_message="changelog: test",
            policy_versions=_policy_versions(),
            upsert_versions=upserts,
            now_iso="2026-07-15T00:00:00Z",
        )

    def test_first_write_creates_schema_header_and_backfills(self):
        client = FakeGitHubClient(initial=None)
        stats = self._sync(client, upserts=[_policy_versions()[0]])
        doc = json.loads(client.file.decode("utf-8"))
        self.assertEqual(doc["schema"], 1)
        self.assertEqual(doc["product"], "gyroflow-niyien")
        self.assertEqual(stats["added"], 1)
        self.assertEqual(stats["backfilled"], 1)  # 1.6.9-ni.1 self-healed
        by_version = {e["version"]: e for e in doc["entries"]}
        self.assertEqual(by_version["1.7.0-ni.2"]["published_at"], "2026-07-15T00:00:00Z")
        self.assertEqual(by_version["1.6.9-ni.1"]["published_at"], "")
        self.assertEqual(by_version["1.7.0-ni.2"]["changelogs"], {"zh": "七"})

    def test_conflict_retries_once_then_succeeds(self):
        client = FakeGitHubClient(initial=None, conflicts=1)
        stats = self._sync(client, upserts=[_policy_versions()[0]])
        self.assertEqual(client.put_calls, 2)
        self.assertEqual(stats["added"], 1)

    def test_conflict_twice_raises(self):
        client = FakeGitHubClient(initial=None, conflicts=2)
        with self.assertRaises(RuntimeError):
            self._sync(client, upserts=[_policy_versions()[0]])

    def test_migration_is_idempotent(self):
        client = FakeGitHubClient(initial=None)
        first = self._sync(client)  # backfill-only, like migrate_changelog_archive
        self.assertEqual(first["backfilled"], 2)
        puts_after_first = client.put_calls
        second = self._sync(client)
        self.assertEqual(second, {"added": 0, "updated": 0, "backfilled": 0})
        self.assertEqual(client.put_calls, puts_after_first)  # no-op: no PUT

    def test_republish_keeps_first_published_at(self):
        client = FakeGitHubClient(initial=None)
        self._sync(client, upserts=[_policy_versions()[0]])
        edited = dict(_policy_versions()[0], changelog="seven edited")
        stats = archive.sync_archive(
            client,
            owner="NiYien", repo="docs", branch="main", path="changelog/archive.json",
            commit_message="changelog: test 2",
            policy_versions=_policy_versions(),
            upsert_versions=[edited],
            now_iso="2026-08-01T00:00:00Z",
        )
        self.assertEqual(stats["updated"], 1)
        doc = json.loads(client.file.decode("utf-8"))
        by_version = {e["version"]: e for e in doc["entries"]}
        self.assertEqual(by_version["1.7.0-ni.2"]["changelog"], "seven edited")
        self.assertEqual(by_version["1.7.0-ni.2"]["published_at"], "2026-07-15T00:00:00Z")

    def test_foreign_document_starts_fresh(self):
        client = FakeGitHubClient(initial=b"not json at all")
        stats = self._sync(client)
        self.assertEqual(stats["backfilled"], 2)
        doc = json.loads(client.file.decode("utf-8"))
        self.assertEqual(doc["schema"], 1)


class GetContentsNoAnonymousFallbackTests(unittest.TestCase):
    def test_missing_file_404_returns_none_with_single_authed_request(self):
        """A missing archive.json is a normal 404 -> None. It must NOT go
        through GitHubClient._get, whose drop-token-and-retry-anonymously
        fallback can die on the per-IP anonymous rate limit (the 403
        'rate limit exceeded' seen on first migration)."""
        import unittest.mock as mock
        from distribution.control_center.backend.github import GitHubClient

        client = GitHubClient(owner="NiYien", repo="docs", token="tok")
        resp = mock.Mock(status_code=404)
        with mock.patch(
            "distribution.control_center.backend.github.requests.get",
            return_value=resp,
        ) as get:
            out = client.get_contents(path="changelog/archive.json", ref="main")
        self.assertIsNone(out)
        self.assertEqual(get.call_count, 1)
        self.assertIn(
            "Bearer tok", get.call_args.kwargs["headers"].get("Authorization", "")
        )


class PublishDegradationTests(unittest.TestCase):
    def test_archive_failure_returns_warning_string(self):
        from distribution.control_center.backend.api import Api

        api = Api.__new__(Api)  # skip __init__ side effects
        policy_json = json.dumps({"versions": _policy_versions()})

        class Boom:
            def get_contents(self, *a, **k):
                raise RuntimeError("network down")

        import unittest.mock as mock
        with mock.patch.object(Api, "_get_publish_secret", return_value="tok"), \
             mock.patch("distribution.control_center.backend.api.GitHubClient",
                        return_value=Boom()):
            note = api._sync_changelog_archive_after_publish(
                {"changelog_archive": {"owner": "NiYien", "repo": "docs",
                                       "branch": "main", "path": "changelog/archive.json"}},
                policy_json,
                "1.7.0-ni.2",
            )
        self.assertIn("changelog 存档写入失败", note)
        self.assertIn("network down", note)

    def test_empty_version_is_a_silent_no_op(self):
        from distribution.control_center.backend.api import Api

        api = Api.__new__(Api)
        self.assertEqual(
            api._sync_changelog_archive_after_publish({}, "{}", ""), ""
        )


if __name__ == "__main__":
    unittest.main()
