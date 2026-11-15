"""Tests for the env-upsert diagnostics hardening:

- `vercel._raise_for_status` folds the response body into the HTTPError so the
  real Vercel error reason survives (instead of the opaque "400 Client Error").
- `Api._is_retryable_http_error` treats deterministic 4xx (except 408/429) as
  non-retryable so the upsert fails fast instead of burning the backoff window.
"""

import json
import unittest
from unittest.mock import Mock, patch

import requests

from distribution.control_center.backend.api import Api
from distribution.control_center.backend import vercel


class _FakeResponse:
    def __init__(self, status_code, *, reason="", url="https://api.vercel.com/x", text=""):
        self.status_code = status_code
        self.reason = reason
        self.url = url
        self.text = text


class RaiseForStatusTests(unittest.TestCase):
    def test_ok_does_not_raise(self):
        vercel._raise_for_status(_FakeResponse(200))
        vercel._raise_for_status(_FakeResponse(201))

    def test_4xx_includes_body(self):
        resp = _FakeResponse(
            400, reason="Bad Request",
            text='{"error":{"code":"env_too_large","message":"too big"}}',
        )
        with self.assertRaises(requests.HTTPError) as ctx:
            vercel._raise_for_status(resp)
        msg = str(ctx.exception)
        self.assertIn("400 Bad Request", msg)
        self.assertIn("env_too_large", msg)        # the real reason survives
        self.assertIs(ctx.exception.response, resp)  # status stays inspectable

    def test_long_body_truncated(self):
        resp = _FakeResponse(400, reason="Bad Request", text="x" * 5000)
        with self.assertRaises(requests.HTTPError) as ctx:
            vercel._raise_for_status(resp)
        # Body capped to ~1000 chars + ellipsis, not the full 5000.
        self.assertLess(len(str(ctx.exception)), 1200)

    def test_empty_body_no_trailing_separator(self):
        resp = _FakeResponse(404, reason="Not Found", text="")
        with self.assertRaises(requests.HTTPError) as ctx:
            vercel._raise_for_status(resp)
        self.assertNotIn("::", str(ctx.exception))


class IsRetryableHttpErrorTests(unittest.TestCase):
    def _err(self, status):
        return requests.HTTPError(response=_FakeResponse(status))

    def test_400_not_retryable(self):
        self.assertFalse(Api._is_retryable_http_error(self._err(400)))

    def test_403_404_422_not_retryable(self):
        for s in (403, 404, 409, 422):
            self.assertFalse(Api._is_retryable_http_error(self._err(s)), s)

    def test_408_and_429_retryable(self):
        self.assertTrue(Api._is_retryable_http_error(self._err(408)))
        self.assertTrue(Api._is_retryable_http_error(self._err(429)))

    def test_5xx_retryable(self):
        for s in (500, 502, 503):
            self.assertTrue(Api._is_retryable_http_error(self._err(s)), s)

    def test_no_response_is_retryable(self):
        # Connection reset / timeout — no HTTP response attached.
        self.assertTrue(Api._is_retryable_http_error(requests.ConnectionError("reset")))
        self.assertTrue(Api._is_retryable_http_error(Exception("generic")))


class PolicyEnvSizeTests(unittest.TestCase):
    def test_compacts_policy_at_http_boundary_without_changing_content(self):
        policy = {
            "auto_version": "1.7.0-ni.173",
            "versions": [
                {
                    "version": f"1.7.0-ni.{number}",
                    "changelogs": {"zh": "第一行\n  保留缩进", "en": "Keep  both spaces"},
                    "packages": {f"asset-{i}": {"size": i, "sha256": "a" * 64} for i in range(50)},
                }
                for number in range(173, 163, -1)
            ],
            "hidden_plugins": [{"kind": "artifact", "run_id": 123}],
        }
        formatted = json.dumps(policy, ensure_ascii=False, indent=2)
        other_env = "  preserve this value\n"
        mapping = {"NIYIEN_RELEASE_POLICY_JSON": formatted, "OTHER_ENV": other_env}
        client = vercel.VercelClient("test-token", "test-project")
        response = Mock(status_code=200)
        response.json.return_value = {"created": []}
        with patch.object(vercel.requests, "post", return_value=response) as post:
            self.assertEqual(client.upsert_envs(mapping), {"created": []})

        post.assert_called_once()
        body = post.call_args.kwargs["json"]
        sent_policy, sent_other = body
        self.assertGreater(len(formatted.encode("utf-8")), 65536)
        self.assertLess(len(sent_policy["value"].encode("utf-8")), 65536)
        self.assertEqual(json.loads(sent_policy["value"]), policy)
        self.assertEqual(sent_policy["target"], ["production", "preview", "development"])
        self.assertEqual(sent_policy["type"], "encrypted")
        self.assertEqual(sent_other["value"], other_env)
        self.assertEqual(mapping["NIYIEN_RELEASE_POLICY_JSON"], formatted)


if __name__ == "__main__":
    unittest.main()
