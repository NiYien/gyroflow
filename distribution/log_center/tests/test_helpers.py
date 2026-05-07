"""Tests for log_center backend helpers."""

from __future__ import annotations

import sys
from pathlib import Path
from types import SimpleNamespace

_PKG = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_PKG))

from backend import helpers  # noqa: E402


def test_clipboard_set_uses_pyperclip_first_on_windows(monkeypatch):
    copied = {}

    fake_pyperclip = SimpleNamespace(copy=lambda text: copied.setdefault("text", text))
    monkeypatch.setitem(sys.modules, "pyperclip", fake_pyperclip)
    monkeypatch.setattr(helpers.sys, "platform", "win32")

    mechanism = helpers.clipboard_set("hello")

    assert mechanism == "pyperclip"
    assert copied["text"] == "hello"


def test_clipboard_set_falls_back_to_file_when_pyperclip_fails_on_windows(tmp_path, monkeypatch):
    def fail_copy(text):
        raise RuntimeError("clipboard unavailable")

    class FakeTk:
        def withdraw(self):
            pass

        def clipboard_clear(self):
            pass

        def clipboard_append(self, text):
            pass

        def update(self):
            pass

        def destroy(self):
            pass

    fake_pyperclip = SimpleNamespace(copy=fail_copy)
    fake_tkinter = SimpleNamespace(Tk=FakeTk)
    monkeypatch.setitem(sys.modules, "pyperclip", fake_pyperclip)
    monkeypatch.setitem(sys.modules, "tkinter", fake_tkinter)
    monkeypatch.setattr(helpers.sys, "platform", "win32")

    mechanism = helpers.clipboard_set("fallback text", fallback_dir=tmp_path)

    assert mechanism.startswith("file:")
    assert Path(mechanism[5:]).read_text(encoding="utf-8") == "fallback text"
