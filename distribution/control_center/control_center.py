#!/usr/bin/env python3
"""NiYien Control Center — pywebview entry point.

Starts a native window hosting frontend/index.html and exposes the backend
API to JS via pywebview's bridge. Real Vercel/GitHub calls live in
`backend/api.py`.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import webview

# Ensure 'backend' is importable when run from any cwd.
HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))

from backend.api import Api  # noqa: E402

FRONTEND_DIR = HERE / "frontend"


def main() -> None:
    if not FRONTEND_DIR.exists():
        print(f"[FATAL] frontend dir missing: {FRONTEND_DIR}", file=sys.stderr)
        sys.exit(1)

    # Disable HTTP caching on frontend assets so edits show up without
    # needing a manual Ctrl+R in the WebView2 window.
    try:
        import bottle  # pywebview's bundled static server
        @bottle.hook("after_request")
        def _no_cache():
            bottle.response.set_header("Cache-Control", "no-store, max-age=0")
            bottle.response.set_header("Pragma", "no-cache")
    except Exception:
        pass

    api = Api()
    window = webview.create_window(
        title="NiYien 发布中心",
        url=str(FRONTEND_DIR / "index.html"),
        js_api=api,
        width=1180,
        height=780,
        min_size=(900, 620),
    )

    # Kill any in-flight publish subprocesses on window close so we don't
    # leak file locks (Windows holds onto the .zip mid-download otherwise)
    # or background uploads that would race with the next launch.
    def _on_closing():
        try:
            from backend.pan123 import TASKS
            n = TASKS.cancel_all(kill_after_s=3.0)
            if n:
                print(f"[shutdown] terminated {n} pan123 task(s)", file=sys.stderr)
        except Exception as exc:
            print(f"[shutdown] cancel_all failed: {exc}", file=sys.stderr)

    window.events.closing += _on_closing

    debug = os.environ.get("CONTROL_CENTER_DEBUG", "").strip() == "1"
    webview.start(debug=debug)


if __name__ == "__main__":
    main()
