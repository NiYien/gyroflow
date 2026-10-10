"""Tests for the docs-repo supported-camera list sync
(control-center-supported-cameras).

Covers the pure payload helpers in backend/supported_cameras.py and the
GET-compare-PUT sync flow against fake GitHub clients:

- DISPLAY_NAMES carries the full mapping migrated from the retired docs
  script (Sony 37 / Lumix 7 / Leica 1)
- build_cameras_payload maps keys, preserves vendor-file order, and
  cross-checks vendors against BRANDS in both directions
- payload_equivalent ignores generated_at (no date-only docs commits)
- dumps_cameras emits the byte format the docs page already serves
- sync_supported_cameras: no PUT when unchanged, first write without sha,
  one retry on a stale-sha conflict, raise after the second conflict
- local-clone integration: payload built from ../niyien-lens-data matches
  the docs repo's live cameras.json (skipped when clones are absent)
"""

import base64
import json
import unittest
from pathlib import Path

from distribution.control_center.backend import supported_cameras as sc
from distribution.control_center.backend.github import ContentsConflictError


def _vendor(models):
    return {"models": {k: {"sw": 35.9} for k in models}}


def _full_vendor_set(sony_models=("ILCE-1", "ZV1")):
    data = {stem: _vendor([f"{stem.upper()}-CAM"]) for stem, _ in sc.BRANDS}
    data["sony"] = _vendor(sony_models)
    data["apple"] = {"readout": {"data": {"iPhone 16 Pro Max 24mm": [-2.4]}}}
    return data


class DisplayNamesTests(unittest.TestCase):
    def test_mapping_counts_match_migrated_script(self):
        self.assertEqual(len(sc.DISPLAY_NAMES["sony"]), 37)
        self.assertEqual(len(sc.DISPLAY_NAMES["lumix"]), 7)
        self.assertEqual(len(sc.DISPLAY_NAMES["leica"]), 1)

    def test_brand_registry_includes_apple(self):
        self.assertEqual(len(sc.BRANDS), 13)
        self.assertEqual(sc.BRANDS[0], ("sony", "Sony"))
        self.assertIn(("apple", "Apple"), sc.BRANDS)


class BuildPayloadTests(unittest.TestCase):
    def test_maps_and_passes_through_in_order(self):
        data = _full_vendor_set(sony_models=("ILCE-7SM3", "UNKNOWN-KEY", "RX1RM2"))
        doc = sc.build_cameras_payload(data, "2026-07-15")
        self.assertEqual(doc["generated_at"], "2026-07-15")
        self.assertEqual([b["id"] for b in doc["brands"]], [stem for stem, _ in sc.BRANDS])
        sony = doc["brands"][0]
        self.assertEqual(sony["name"], "Sony")
        self.assertEqual(sony["models"], ["α7S III", "UNKNOWN-KEY", "RX1R II"])

    def test_unregistered_vendor_rejected(self):
        data = _full_vendor_set()
        data["newvendor"] = _vendor(["X1"])
        with self.assertRaises(ValueError) as ctx:
            sc.build_cameras_payload(data, "2026-07-15")
        self.assertIn("newvendor", str(ctx.exception))

    def test_missing_vendor_rejected(self):
        data = _full_vendor_set()
        del data["ricoh"]
        with self.assertRaises(ValueError) as ctx:
            sc.build_cameras_payload(data, "2026-07-15")
        self.assertIn("ricoh", str(ctx.exception))

    def test_empty_models_rejected(self):
        data = _full_vendor_set()
        data["zcam"] = {"models": {}}
        with self.assertRaises(ValueError) as ctx:
            sc.build_cameras_payload(data, "2026-07-15")
        self.assertIn("zcam", str(ctx.exception))

    def test_apple_readout_labels_become_unique_models_in_order(self):
        data = _full_vendor_set()
        data["apple"] = {"readout": {"data": {
            "iPhone 16 Pro Max 24mm": [-2.4],
            "iPhone 16 Pro Max 13mm": [-5.8],
            "iPhone 16 Pro 24mm": [None],
            "iPhone Air 26mm": [None],
            "iPhone 16 Pro Max 120mm": [-5.5],
        }}}
        doc = sc.build_cameras_payload(data, "2026-10-09")
        self.assertEqual(doc["brands"][-1], {
            "id": "apple", "name": "Apple",
            "models": ["iPhone 16 Pro Max", "iPhone 16 Pro", "iPhone Air"],
        })

    def test_older_tag_without_apple_still_builds(self):
        data = _full_vendor_set()
        del data["apple"]
        doc = sc.build_cameras_payload(data, "2026-10-09")
        self.assertEqual(len(doc["brands"]), 12)
        self.assertNotIn("apple", [b["id"] for b in doc["brands"]])

    def test_empty_or_invalid_apple_readout_rejected(self):
        for vendor in ({}, {"readout": None}, {"readout": {}},
                       {"readout": {"data": {}}}, {"readout": {"data": []}}):
            with self.subTest(vendor=vendor):
                data = _full_vendor_set()
                data["apple"] = vendor
                with self.assertRaisesRegex(ValueError, "apple.json.*readout.data"):
                    sc.build_cameras_payload(data, "2026-10-09")

    def test_invalid_apple_lens_label_rejected(self):
        data = _full_vendor_set()
        data["apple"] = {"readout": {"data": {"iPhone 16 Pro Max": [None]}}}
        with self.assertRaisesRegex(ValueError, "invalid model/lens label"):
            sc.build_cameras_payload(data, "2026-10-09")

    def test_bundled_camera_db_builds_with_apple(self):
        camera_db = Path(__file__).resolve().parents[3] / "resources" / "camera_db"
        data = {
            p.stem: json.loads(p.read_text(encoding="utf-8"))
            for p in sorted(camera_db.glob("*.json"))
        }
        doc = sc.build_cameras_payload(data, "2026-10-09")
        apple = next(b for b in doc["brands"] if b["id"] == "apple")
        self.assertIn("iPhone 16 Pro Max", apple["models"])
        self.assertEqual(len(apple["models"]), len(set(apple["models"])))
        self.assertTrue(all(not model.endswith("mm") for model in apple["models"]))


class EquivalenceAndFormatTests(unittest.TestCase):
    def test_equivalence_ignores_generated_at(self):
        data = _full_vendor_set()
        a = sc.build_cameras_payload(data, "2026-07-15")
        b = sc.build_cameras_payload(data, "2026-08-01")
        self.assertTrue(sc.payload_equivalent(a, b))

    def test_equivalence_detects_model_change(self):
        a = sc.build_cameras_payload(_full_vendor_set(("ILCE-1",)), "2026-07-15")
        b = sc.build_cameras_payload(_full_vendor_set(("ILCE-1", "ZV1")), "2026-07-15")
        self.assertFalse(sc.payload_equivalent(a, b))

    def test_dumps_format(self):
        doc = sc.build_cameras_payload(_full_vendor_set(("ILCE-7SM3",)), "2026-07-15")
        raw = sc.dumps_cameras(doc)
        self.assertFalse(raw.startswith(b"\xef\xbb\xbf"))
        self.assertTrue(raw.endswith(b"}\n"))
        self.assertIn("α7S III".encode("utf-8"), raw)  # ensure_ascii off
        self.assertIn(b'\n  "brands": [', raw)  # indent 2
        self.assertEqual(json.loads(raw.decode("utf-8")), doc)


class FakeLensClient:
    def __init__(self, vendor_jsons):
        self.vendor_jsons = vendor_jsons
        self.requested_refs = set()

    def get_contents(self, owner=None, repo=None, *, path, ref=""):
        self.requested_refs.add(ref)
        if path == "camera_db":
            return [
                {"name": f"{stem}.json", "type": "file"}
                for stem in self.vendor_jsons
            ]
        stem = path.rsplit("/", 1)[-1][:-5]
        raw = json.dumps(self.vendor_jsons[stem]).encode("utf-8")
        return {"content": base64.b64encode(raw).decode("ascii"), "sha": f"blob-{stem}"}


class FakeDocsClient:
    def __init__(self, existing_bytes=None, conflicts=0):
        self.existing_bytes = existing_bytes
        self.conflicts_remaining = conflicts
        self.put_calls = []

    def get_contents(self, owner=None, repo=None, *, path, ref=""):
        if self.existing_bytes is None:
            return None
        return {
            "content": base64.b64encode(self.existing_bytes).decode("ascii"),
            "sha": "docs-sha",
        }

    def put_contents(self, owner=None, repo=None, *, path, message, content_bytes, sha="", branch=""):
        if self.conflicts_remaining > 0:
            self.conflicts_remaining -= 1
            raise ContentsConflictError("stale sha")
        self.put_calls.append({"sha": sha, "branch": branch, "bytes": content_bytes, "message": message})
        return {"content": {"sha": "new-sha"}}


def _run_sync(lens_client, docs_client):
    return sc.sync_supported_cameras(
        lens_client,
        docs_client,
        lens_owner="NiYien",
        lens_repo="niyien-lens-data",
        lens_ref="data-v20260715.1",
        docs_owner="NiYien",
        docs_repo="docs",
        docs_branch="main",
        docs_path="cameras/cameras.json",
        commit_message="cameras: sync supported-camera list (data-v20260715.1)",
        now_date="2026-07-15",
    )


class SyncTests(unittest.TestCase):
    def test_first_write_without_sha(self):
        lens = FakeLensClient(_full_vendor_set())
        docs = FakeDocsClient(existing_bytes=None)
        result = _run_sync(lens, docs)
        self.assertTrue(result["changed"])
        self.assertEqual(result["brands"], 13)
        self.assertEqual(result["tag"], "data-v20260715.1")
        self.assertEqual(len(docs.put_calls), 1)
        self.assertEqual(docs.put_calls[0]["sha"], "")
        self.assertEqual(docs.put_calls[0]["branch"], "main")
        self.assertIn("data-v20260715.1", lens.requested_refs)
        written = json.loads(docs.put_calls[0]["bytes"].decode("utf-8"))
        self.assertEqual(written["brands"][-1]["models"], ["iPhone 16 Pro Max"])

    def test_unchanged_content_skips_put(self):
        data = _full_vendor_set()
        # Existing file has a different (older) date but identical brands.
        existing = sc.dumps_cameras(sc.build_cameras_payload(data, "2020-01-01"))
        lens = FakeLensClient(data)
        docs = FakeDocsClient(existing_bytes=existing)
        result = _run_sync(lens, docs)
        self.assertFalse(result["changed"])
        self.assertEqual(docs.put_calls, [])

    def test_changed_content_puts_with_sha(self):
        existing = sc.dumps_cameras(
            sc.build_cameras_payload(_full_vendor_set(("ILCE-1",)), "2026-07-01")
        )
        lens = FakeLensClient(_full_vendor_set(("ILCE-1", "ZV1")))
        docs = FakeDocsClient(existing_bytes=existing)
        result = _run_sync(lens, docs)
        self.assertTrue(result["changed"])
        self.assertEqual(len(docs.put_calls), 1)
        self.assertEqual(docs.put_calls[0]["sha"], "docs-sha")
        written = json.loads(docs.put_calls[0]["bytes"].decode("utf-8"))
        self.assertEqual(written["generated_at"], "2026-07-15")
        self.assertEqual(written["brands"][0]["models"], ["α1", "ZV-1"])

    def test_conflict_retries_once(self):
        lens = FakeLensClient(_full_vendor_set())
        docs = FakeDocsClient(existing_bytes=None, conflicts=1)
        result = _run_sync(lens, docs)
        self.assertTrue(result["changed"])
        self.assertEqual(len(docs.put_calls), 1)

    def test_double_conflict_raises(self):
        lens = FakeLensClient(_full_vendor_set())
        docs = FakeDocsClient(existing_bytes=None, conflicts=2)
        with self.assertRaises(RuntimeError):
            _run_sync(lens, docs)
        self.assertEqual(docs.put_calls, [])

    def test_corrupt_existing_file_is_overwritten(self):
        lens = FakeLensClient(_full_vendor_set())
        docs = FakeDocsClient(existing_bytes=b"not json at all")
        result = _run_sync(lens, docs)
        self.assertTrue(result["changed"])
        self.assertEqual(len(docs.put_calls), 1)
        self.assertEqual(docs.put_calls[0]["sha"], "docs-sha")


_GITHUB_DIR = Path(__file__).resolve().parents[4]
_LENS_DB = _GITHUB_DIR / "niyien-lens-data" / "camera_db"
_DOCS_JSON = _GITHUB_DIR / "docs" / "cameras" / "cameras.json"


@unittest.skipUnless(
    _LENS_DB.is_dir() and _DOCS_JSON.is_file(),
    "local niyien-lens-data / docs clones not present",
)
class LocalCloneEquivalenceTests(unittest.TestCase):
    """Payload built from the local lens-data clone matches the docs repo's
    committed cameras.json (brands content, generated_at excluded)."""

    def test_local_clone_matches_live_snapshot(self):
        vendor_jsons = {
            p.stem: json.loads(p.read_text(encoding="utf-8"))
            for p in sorted(_LENS_DB.glob("*.json"))
        }
        built = sc.build_cameras_payload(vendor_jsons, "0000-00-00")
        live = json.loads(_DOCS_JSON.read_text(encoding="utf-8"))
        self.assertTrue(sc.payload_equivalent(built, live))


if __name__ == "__main__":
    unittest.main()
