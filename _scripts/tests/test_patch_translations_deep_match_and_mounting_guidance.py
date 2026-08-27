import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PATCHER_PATH = ROOT / "_scripts" / "patch_translations_deep_match_and_mounting_guidance.py"


def load_patcher():
    spec = importlib.util.spec_from_file_location("deep_match_guidance_patcher", PATCHER_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load {PATCHER_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class TranslationGuidancePatcherTests(unittest.TestCase):
    def setUp(self):
        self.patcher = load_patcher()

    def test_remove_message_matches_crlf_multiline_source(self):
        raw = (
            "<context>\r\n"
            "    <name>RenderQueue</name>\r\n"
            "    <message>\r\n"
            "        <source>First line\r\nSecond line</source>\r\n"
            "        <translation>Old translation</translation>\r\n"
            "    </message>\r\n"
            "</context>\r\n"
        )

        result = self.patcher.remove_message(
            raw,
            "RenderQueue",
            "First line\nSecond line",
            "\r\n",
        )

        self.assertNotIn("<message>", result)

    def test_message_block_uses_crlf_inside_multiline_text(self):
        result = self.patcher.message_block(
            "\r\n",
            "../../src/ui/RenderQueue.qml",
            651,
            "First line\nSecond line",
            "First translation\nSecond translation",
        )

        self.assertIn("<source>First line\r\nSecond line</source>", result)
        self.assertIn(
            "<translation>First translation\r\nSecond translation</translation>",
            result,
        )
        self.assertNotIn("\n", result.replace("\r\n", ""))


if __name__ == "__main__":
    unittest.main()
