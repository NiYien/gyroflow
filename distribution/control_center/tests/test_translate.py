"""Tests for backend/translate.py.

Mocks `requests.post` at the import boundary so the tests never touch a
real LLM endpoint.
"""

import json
import unittest
from unittest.mock import patch

from distribution.control_center.backend import translate as translate_module
from distribution.control_center.backend.translate import (
    TARGET_LANGS,
    TranslateConfigError,
    TranslateHTTPError,
    TranslateParseError,
    _parse_translation_payload,
    _strip_code_fence,
    translate_to_all,
)


_VALID_CFG = {
    "translate_api_base": "https://api.example.com/v1",
    "translate_api_key": "sk-test",
    "translate_model": "test-model",
    "network_proxy": "",
}


def _make_envelope(content: str, finish_reason: str = "stop") -> dict:
    return {
        "choices": [
            {
                "message": {"role": "assistant", "content": content},
                "finish_reason": finish_reason,
            }
        ]
    }


class _FakeResponse:
    def __init__(self, status_code: int, payload, content_type: str = "application/json"):
        self.status_code = status_code
        self._payload = payload
        self.headers = {"Content-Type": content_type}
        if isinstance(payload, (dict, list)):
            self.text = json.dumps(payload)
        else:
            self.text = str(payload)

    def json(self):
        if isinstance(self._payload, (dict, list)):
            return self._payload
        raise ValueError("not json")


class StripCodeFenceTests(unittest.TestCase):
    def test_plain_json_unchanged(self):
        body = '{"en": "x"}'
        self.assertEqual(_strip_code_fence(body), body)

    def test_strips_json_fence(self):
        body = '```json\n{"en": "x"}\n```'
        self.assertEqual(_strip_code_fence(body), '{"en": "x"}')

    def test_strips_bare_fence(self):
        body = '```\n{"en": "x"}\n```'
        self.assertEqual(_strip_code_fence(body), '{"en": "x"}')

    def test_strips_with_surrounding_whitespace(self):
        body = '   ```json\n{"en": "x"}\n```   '
        self.assertEqual(_strip_code_fence(body), '{"en": "x"}')


class ParseTranslationPayloadTests(unittest.TestCase):
    def _payload(self) -> dict:
        return {code: f"hello-{code}" for code in TARGET_LANGS}

    def test_all_languages_present(self):
        out = _parse_translation_payload(json.dumps(self._payload()))
        self.assertEqual(set(out.keys()), set(TARGET_LANGS))
        self.assertEqual(out["en"], "hello-en")
        self.assertEqual(out["pt"], "hello-pt")

    def test_code_fence_tolerated(self):
        fenced = "```json\n" + json.dumps(self._payload()) + "\n```"
        out = _parse_translation_payload(fenced)
        self.assertEqual(set(out.keys()), set(TARGET_LANGS))

    def test_missing_key_raises(self):
        payload = self._payload()
        del payload["ja"]
        with self.assertRaises(TranslateParseError) as ctx:
            _parse_translation_payload(json.dumps(payload))
        self.assertIn("ja", str(ctx.exception))

    def test_empty_value_treated_as_missing(self):
        payload = self._payload()
        payload["ko"] = "   "
        with self.assertRaises(TranslateParseError) as ctx:
            _parse_translation_payload(json.dumps(payload))
        self.assertIn("ko", str(ctx.exception))

    def test_non_string_value_raises(self):
        payload = self._payload()
        payload["en"] = ["wrong", "type"]
        with self.assertRaises(TranslateParseError) as ctx:
            _parse_translation_payload(json.dumps(payload))
        self.assertIn("en", str(ctx.exception))

    def test_invalid_json_raises(self):
        with self.assertRaises(TranslateParseError):
            _parse_translation_payload("not actually json {")

    def test_non_object_raises(self):
        with self.assertRaises(TranslateParseError):
            _parse_translation_payload(json.dumps(["a", "b"]))


class AssembleSseContentTests(unittest.TestCase):
    def test_concatenates_delta_chunks(self):
        from distribution.control_center.backend.translate import _assemble_sse_content
        body = (
            'data: {"choices":[{"delta":{"content":"Hello "}}]}\n\n'
            'data: {"choices":[{"delta":{"content":"world"},"finish_reason":"stop"}]}\n\n'
            'data: [DONE]\n'
        )
        content, reason = _assemble_sse_content(body)
        self.assertEqual(content, "Hello world")
        self.assertEqual(reason, "stop")

    def test_skips_empty_choices(self):
        from distribution.control_center.backend.translate import _assemble_sse_content
        body = (
            'data: {"choices":[]}\n\n'                                   # heartbeat
            'data: {"choices":[{"delta":{"content":"ok"}}]}\n\n'
        )
        content, reason = _assemble_sse_content(body)
        self.assertEqual(content, "ok")
        self.assertEqual(reason, "")

    def test_falls_back_to_message_content(self):
        # Some servers emit a single non-streamed `message.content` even
        # when the response is wrapped in SSE framing.
        from distribution.control_center.backend.translate import _assemble_sse_content
        body = 'data: {"choices":[{"message":{"content":"final"}}]}\n\n'
        content, _ = _assemble_sse_content(body)
        self.assertEqual(content, "final")

    def test_ignores_malformed_chunks(self):
        from distribution.control_center.backend.translate import _assemble_sse_content
        body = (
            'data: not json\n\n'
            'data: {"choices":[{"delta":{"content":"ok"}}]}\n\n'
        )
        content, _ = _assemble_sse_content(body)
        self.assertEqual(content, "ok")

    def test_captures_finish_reason_length(self):
        # Length cutoff = truncated response. Caller must reject these.
        from distribution.control_center.backend.translate import _assemble_sse_content
        body = (
            'data: {"choices":[{"delta":{"content":"partial"},"finish_reason":"length"}]}\n\n'
        )
        content, reason = _assemble_sse_content(body)
        self.assertEqual(content, "partial")
        self.assertEqual(reason, "length")


class SseEndToEndTests(unittest.TestCase):
    def test_sse_truncated_finish_reason_raises(self):
        # SSE path must mirror the non-stream path's finish_reason check —
        # length-cutoff produces partial output and we'd rather fail than
        # ship a half-translated changelog.
        partial = json.dumps({"en": "partial"})  # only en, missing the other 7
        sse_body = (
            f'data: {{"choices":[{{"delta":{{"content":{json.dumps(partial)}}},"finish_reason":"length"}}]}}\n\n'
        )

        class _SseResp:
            status_code = 200
            text = sse_body
            headers = {"Content-Type": "text/event-stream"}

            def json(self):
                raise ValueError("SSE body")

        with patch.object(translate_module.requests, "post", return_value=_SseResp()):
            with self.assertRaises(TranslateParseError) as ctx:
                translate_to_all("中文", cfg=_VALID_CFG)
        self.assertIn("length", str(ctx.exception))

    def test_sse_response_parses_via_assemble_path(self):
        # Some OpenAI-compatible proxies ignore `stream: false` and emit
        # text/event-stream regardless. Verify we still extract the JSON
        # translation map from the assembled delta stream.
        payload = json.dumps({code: f"x-{code}" for code in TARGET_LANGS})
        sse_body = (
            f'data: {{"choices":[{{"delta":{{"content":{json.dumps(payload[:10])}}}}}]}}\n\n'
            f'data: {{"choices":[{{"delta":{{"content":{json.dumps(payload[10:])}}}}}]}}\n\n'
            'data: [DONE]\n'
        )

        class _SseResp:
            status_code = 200
            text = sse_body
            headers = {"Content-Type": "text/event-stream; charset=utf-8"}

            def json(self):
                raise ValueError("SSE body — should not be called")

        with patch.object(translate_module.requests, "post", return_value=_SseResp()):
            out = translate_to_all("中文", cfg=_VALID_CFG)
        self.assertEqual(set(out.keys()), set(TARGET_LANGS))
        self.assertEqual(out["en"], "x-en")


class TranslateToAllTests(unittest.TestCase):
    def _full_payload(self) -> dict:
        return {code: f"trans-{code}" for code in TARGET_LANGS}

    def test_happy_path_returns_8_keys(self):
        envelope = _make_envelope(json.dumps(self._full_payload()))
        with patch.object(translate_module.requests, "post",
                          return_value=_FakeResponse(200, envelope)) as mock_post:
            out = translate_to_all("中文 release notes", cfg=_VALID_CFG)
        self.assertEqual(set(out.keys()), set(TARGET_LANGS))
        self.assertEqual(out["ja"], "trans-ja")
        # URL got the expected suffix.
        called_url = mock_post.call_args[0][0]
        self.assertEqual(called_url, "https://api.example.com/v1/chat/completions")
        # Bearer header was attached.
        sent_headers = mock_post.call_args[1]["headers"]
        self.assertEqual(sent_headers["Authorization"], "Bearer sk-test")
        # Body has the configured model.
        sent_body = mock_post.call_args[1]["json"]
        self.assertEqual(sent_body["model"], "test-model")

    def test_code_fence_tolerated_end_to_end(self):
        fenced = "```json\n" + json.dumps(self._full_payload()) + "\n```"
        envelope = _make_envelope(fenced)
        with patch.object(translate_module.requests, "post",
                          return_value=_FakeResponse(200, envelope)):
            out = translate_to_all("中文", cfg=_VALID_CFG)
        self.assertEqual(set(out.keys()), set(TARGET_LANGS))

    def test_missing_key_raises_parse_error(self):
        partial = self._full_payload()
        del partial["en"]
        envelope = _make_envelope(json.dumps(partial))
        with patch.object(translate_module.requests, "post",
                          return_value=_FakeResponse(200, envelope)):
            with self.assertRaises(TranslateParseError) as ctx:
                translate_to_all("中文", cfg=_VALID_CFG)
        self.assertIn("en", str(ctx.exception))

    def test_http_error_raises_http_error(self):
        envelope = {"error": "unauthorized"}
        with patch.object(translate_module.requests, "post",
                          return_value=_FakeResponse(401, envelope)):
            with self.assertRaises(TranslateHTTPError) as ctx:
                translate_to_all("中文", cfg=_VALID_CFG)
        self.assertIn("401", str(ctx.exception))

    def test_missing_config_raises_config_error(self):
        for missing_key in ("translate_api_base", "translate_api_key", "translate_model"):
            with self.subTest(missing=missing_key):
                bad_cfg = dict(_VALID_CFG)
                bad_cfg[missing_key] = ""
                with self.assertRaises(TranslateConfigError):
                    translate_to_all("中文", cfg=bad_cfg)

    def test_empty_input_raises(self):
        with self.assertRaises(translate_module.TranslateError):
            translate_to_all("   ", cfg=_VALID_CFG)

    def test_truncated_finish_reason_raises(self):
        envelope = _make_envelope(
            json.dumps(self._full_payload()),
            finish_reason="length",
        )
        with patch.object(translate_module.requests, "post",
                          return_value=_FakeResponse(200, envelope)):
            with self.assertRaises(TranslateParseError) as ctx:
                translate_to_all("中文", cfg=_VALID_CFG)
        self.assertIn("length", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
