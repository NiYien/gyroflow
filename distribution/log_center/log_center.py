#!/usr/bin/env python3
"""NiYien Feedback Log Center — pywebview entry point.

Loads `log_center.config.json`, instantiates the BackendAPI, and opens
a native window pointed at `frontend/index.html`.

Run with `python log_center.py` (Linux/macOS) or via `log_center.pyw`
on Windows for a no-console-window launch.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from backend.config import ConfigError, load_config  # noqa: E402

FRONTEND_DIR = HERE / "frontend"


def _fail(message: str) -> None:
    print(f"[log_center] 致命错误：{message}", file=sys.stderr)
    sys.exit(1)


def main() -> None:
    if not FRONTEND_DIR.exists():
        _fail(f"前端目录缺失：{FRONTEND_DIR}")

    try:
        config = load_config()
    except ConfigError as exc:
        _fail(str(exc))

    # Defer imports until after config validates so missing optional
    # dependencies (boto3 etc.) get reported with a clear stack rather
    # than a silent crash before any window opens.
    try:
        import webview  # type: ignore
    except ImportError:
        _fail("未安装 `pywebview`。请运行：pip install -r requirements.txt")

    from backend.orchestrator import BackendAPI  # noqa: E402

    backend = BackendAPI(config)

    window = webview.create_window(
        title="NiYien 反馈日志中心",
        url=str(FRONTEND_DIR / "index.html"),
        js_api=backend,
        width=1340,
        height=820,
        min_size=(960, 600),
    )

    def _on_closing() -> None:
        try:
            backend.shutdown()
        except Exception as exc:  # noqa: BLE001
            print(f"[log_center] 关闭时出错：{exc}", file=sys.stderr)

    window.events.closing += _on_closing

    debug = os.environ.get("LOG_CENTER_DEBUG", "").strip() == "1"
    webview.start(debug=debug)


if __name__ == "__main__":
    main()
