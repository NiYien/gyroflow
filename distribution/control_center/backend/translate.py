"""LLM-based translation of Chinese release notes to 8 target languages.

Uses an OpenAI-compatible `chat/completions` endpoint so the same client
works against DeepSeek, OpenAI, Azure OpenAI, or any compatible proxy.
A single API call returns a strict JSON map of `{lang: text}` for all
8 target languages, preserving Markdown formatting and technical terms.
"""

from __future__ import annotations

import json
import re
from typing import Optional

import requests

from . import config as config_module
from .helpers import build_proxy_mapping


TARGET_LANGS: tuple[str, ...] = ("en", "ja", "ko", "de", "fr", "es", "ru", "pt")

_LANG_DISPLAY = {
    "en": "English",
    "ja": "Japanese",
    "ko": "Korean",
    "de": "German",
    "fr": "French",
    "es": "Spanish",
    "ru": "Russian",
    "pt": "Portuguese",
}

# Prompt is intentionally embedded as a module constant so versioning is
# explicit. Bump TRANSLATE_PROMPT_VERSION if the prompt changes in a way
# callers need to detect (e.g. for cache invalidation).
TRANSLATE_PROMPT_VERSION = 1

_SYSTEM_PROMPT = (
    "You are a release-notes translator for the Gyroflow video stabilization "
    "application. Translate Chinese release notes into the requested target "
    "languages while preserving the original meaning, terseness, and Markdown "
    "structure."
)

_USER_PROMPT_TEMPLATE = """Translate the Chinese release notes below into these target languages: {targets_human}.

Rules:
1. Preserve Markdown verbatim: list markers (-, *), **bold**, `code`, links, headings.
2. Do NOT translate version numbers (e.g. v1.6.4), file paths, environment variable names, code identifiers, English menu labels in quotes (e.g. "Settings -> Sync"), or technical terms that are conventionally English in software (gyroscope, IMU, sync, RANSAC, FFmpeg, codec names, etc.).
3. Use idiomatic photography/video terminology for each language (e.g. Chinese "稳定化" -> English "stabilization", Japanese "スタビライズ", Korean "스태빌라이즈" etc.).
4. Keep the tone concise and matter-of-fact, matching a software changelog. No marketing fluff.
5. Return STRICT JSON with exactly these keys: {target_keys}. The value of each key is the translated release notes for that language. Do NOT wrap the JSON in a markdown code fence. Do NOT add commentary before or after the JSON object.

Chinese release notes:
<<<
{zh_text}
>>>"""


class TranslateError(Exception):
    """Raised on any failure in the translate pipeline (config missing, HTTP
    error, non-JSON response, missing language keys, etc.).
    """


class TranslateConfigError(TranslateError):
    """One of `translate_api_base / _api_key / _model` is missing."""


class TranslateHTTPError(TranslateError):
    """Underlying HTTP layer reported an error or non-2xx status."""


class TranslateParseError(TranslateError):
    """Response body could not be parsed into the expected JSON map."""


_CODE_FENCE_RE = re.compile(r"^\s*```(?:json)?\s*|\s*```\s*$", re.IGNORECASE)


def _strip_code_fence(text: str) -> str:
    """Strip a leading ```json fence and trailing ``` fence if present.

    Some models like to wrap JSON in a markdown code fence even when the
    prompt forbids it. We strip exactly one leading and one trailing fence
    line — anything fancier is treated as a parse error downstream.
    """
    cleaned = text.strip()
    # Try fenced ```json ... ``` first.
    m = re.fullmatch(
        r"```(?:json)?\s*\n(?P<body>.*?)\n```",
        cleaned,
        flags=re.DOTALL | re.IGNORECASE,
    )
    if m:
        return m.group("body").strip()
    # Otherwise strip leading/trailing single-line fences if they exist.
    cleaned = _CODE_FENCE_RE.sub("", cleaned).strip()
    return cleaned


def _build_user_prompt(zh_text: str) -> str:
    targets_human = ", ".join(f"{code} ({_LANG_DISPLAY[code]})" for code in TARGET_LANGS)
    target_keys = ", ".join(f'"{code}"' for code in TARGET_LANGS)
    return _USER_PROMPT_TEMPLATE.format(
        targets_human=targets_human,
        target_keys=target_keys,
        zh_text=zh_text,
    )


def translate_to_all(
    zh_text: str,
    cfg: Optional[dict] = None,
    *,
    timeout_seconds: float = 60.0,
) -> dict[str, str]:
    """Translate Chinese release notes into all 8 target languages in one call.

    Returns a dict with exactly 8 keys (en/ja/ko/de/fr/es/ru/pt). Raises
    TranslateConfigError / TranslateHTTPError / TranslateParseError on
    failure. Callers are expected to catch TranslateError (the base class).
    """
    text = (zh_text or "").strip()
    if not text:
        raise TranslateError("Chinese source text is empty")

    if cfg is None:
        cfg = config_module.load_config()

    api_base = str(cfg.get("translate_api_base", "") or "").strip().rstrip("/")
    api_key = str(cfg.get("translate_api_key", "") or "").strip()
    model = str(cfg.get("translate_model", "") or "").strip()
    if not api_base or not api_key or not model:
        raise TranslateConfigError(
            "translate_api_base / translate_api_key / translate_model "
            "must all be set in control_center.config.json"
        )

    url = f"{api_base}/chat/completions"
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": _SYSTEM_PROMPT},
            {"role": "user", "content": _build_user_prompt(text)},
        ],
        "temperature": 0.2,
        # Force a single non-streaming JSON response. Some OpenAI-
        # compatible proxies default to SSE (`text/event-stream`) when
        # `stream` is unspecified, which would break our `r.json()` parser.
        "stream": False,
        # `response_format` is optional in OpenAI / DeepSeek and the prompt
        # already enforces JSON. We omit it for maximum compatibility.
    }

    proxies = build_proxy_mapping(cfg.get("network_proxy", ""))
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
    }
    kwargs: dict = {"timeout": timeout_seconds, "headers": headers}
    if proxies:
        kwargs["proxies"] = proxies

    try:
        r = requests.post(url, json=body, **kwargs)
    except requests.RequestException as exc:
        raise TranslateHTTPError(f"HTTP request failed: {exc}") from exc

    if r.status_code >= 400:
        snippet = r.text[:400] if r.text else ""
        raise TranslateHTTPError(
            f"HTTP {r.status_code} from {url}: {snippet}"
        )

    content_type = r.headers.get("Content-Type", "")
    raw_body = r.text or ""
    body_len = len(raw_body)
    # Some OpenAI-compatible proxies ignore `stream: false` and stream
    # anyway. Detect by Content-Type *or* by body-looks-like-SSE (some
    # services even advertise application/json but still emit `data:`
    # frames) and reassemble the streamed `delta.content` chunks.
    looks_like_sse = (
        "text/event-stream" in content_type.lower()
        or raw_body.lstrip().startswith("data:")
    )
    if looks_like_sse:
        message_content, sse_finish_reason = _assemble_sse_content(raw_body)
        if not message_content:
            raise TranslateParseError(
                f"SSE stream contained no assistant content "
                f"(status={r.status_code}, Content-Type={content_type!r}, "
                f"body_len={body_len}). Body starts: {raw_body[:400]!r}"
            )
        # Reject truncation / safety-filter cutoffs — these silently
        # produce partial output (e.g. last few language entries missing
        # or cut mid-sentence), and shipping a half-translated changelog
        # is worse than failing loudly.
        if sse_finish_reason and sse_finish_reason != "stop":
            raise TranslateParseError(
                f"SSE stream ended with finish_reason={sse_finish_reason!r}; "
                f"output is likely truncated (typical cause: max_tokens "
                f"hit or content filter). Try shortening the Chinese "
                f"source or switching to a model with a larger output cap."
            )
        return _parse_translation_payload(message_content)

    try:
        envelope = r.json()
    except ValueError as exc:
        # Surface what the server actually sent so the operator can tell
        # apart a proxy/captive-portal page, an empty body, or a content-
        # type mismatch. Common cause: control_center.config.json sets
        # `network_proxy` to a local proxy that doesn't forward to the
        # configured LLM host, returning HTML/empty.
        raise TranslateParseError(
            f"Response was not JSON (status={r.status_code}, "
            f"Content-Type={content_type!r}, body_len={body_len}): {exc}. "
            f"Body starts: {raw_body[:400]!r}. "
            f"If you see HTML or an empty body, check `network_proxy` in "
            f"control_center.config.json — it may be routing this request "
            f"through a proxy that does not forward to {url}."
        ) from exc

    try:
        choice = envelope["choices"][0]
        message_content = choice["message"]["content"]
        finish_reason = choice.get("finish_reason", "")
    except (KeyError, IndexError, TypeError) as exc:
        raise TranslateParseError(
            f"Response envelope missing choices/message: {exc}"
        ) from exc

    if finish_reason and finish_reason != "stop":
        # Length-cutoff or content-filter responses are unreliable for our
        # full-coverage requirement; bail rather than silently truncate.
        raise TranslateParseError(
            f"Model finish_reason={finish_reason!r}; output may be truncated"
        )

    return _parse_translation_payload(message_content)


def _assemble_sse_content(body: str) -> tuple[str, str]:
    """Concatenate `delta.content` chunks from an OpenAI-style SSE stream.

    Stream format is a sequence of `data: <json>\\n\\n` blocks, where each
    `<json>` is a chat-completion *chunk* shaped like:

        {"choices": [{"delta": {"content": "..."}}], ...}

    A terminal `data: [DONE]` marker ends the stream. Empty `choices`
    arrays (the very first heartbeat-style chunks some proxies emit) are
    skipped silently. Anything that doesn't parse as JSON is also skipped
    — the goal is "give me whatever assistant text I can extract", not
    strict SSE conformance.

    Returns: (assembled_content, finish_reason). `finish_reason` is the
    last non-empty value seen across chunks ("stop" / "length" /
    "content_filter" / etc.) or "" if no chunk carried one. Callers
    should treat anything other than "stop" or "" as a truncated/bad
    response (length cutoff means we have an incomplete translation).
    """
    parts: list[str] = []
    finish_reason = ""
    for line in (body or "").splitlines():
        if not line.startswith("data:"):
            continue
        payload = line[len("data:"):].strip()
        if not payload or payload == "[DONE]":
            continue
        try:
            obj = json.loads(payload)
        except json.JSONDecodeError:
            continue
        try:
            choices = obj.get("choices") or []
            if not choices:
                continue
            first = choices[0]
            # Streaming chunks use `delta.content`; some servers also
            # emit the final aggregated message under `message.content`.
            delta = first.get("delta") or {}
            chunk_text = delta.get("content")
            if chunk_text is None:
                message = first.get("message") or {}
                chunk_text = message.get("content")
            if isinstance(chunk_text, str) and chunk_text:
                parts.append(chunk_text)
            chunk_finish = first.get("finish_reason")
            if isinstance(chunk_finish, str) and chunk_finish:
                finish_reason = chunk_finish
        except (AttributeError, TypeError):
            continue
    return "".join(parts), finish_reason


def _parse_translation_payload(text: str) -> dict[str, str]:
    """Parse the model's JSON answer into a {lang: text} dict.

    Tolerant of an optional ```json ... ``` code fence. Strict about the
    set of keys — exactly TARGET_LANGS, no missing entries.
    """
    cleaned = _strip_code_fence(text)
    if not cleaned:
        raise TranslateParseError("Model returned an empty body")
    try:
        parsed = json.loads(cleaned)
    except json.JSONDecodeError as exc:
        snippet = cleaned[:400]
        raise TranslateParseError(
            f"JSON parse failed: {exc.msg} (body starts: {snippet!r})"
        ) from exc
    if not isinstance(parsed, dict):
        raise TranslateParseError(
            f"Expected JSON object, got {type(parsed).__name__}"
        )

    out: dict[str, str] = {}
    missing: list[str] = []
    for code in TARGET_LANGS:
        value = parsed.get(code)
        if value is None or not isinstance(value, str) or not value.strip():
            missing.append(code)
            continue
        out[code] = value
    if missing:
        raise TranslateParseError(
            f"Response is missing translations for: {', '.join(missing)}"
        )
    return out
