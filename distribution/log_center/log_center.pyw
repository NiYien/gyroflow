#!/usr/bin/env pythonw
"""Windows GUI launcher for log_center — runs under pythonw.exe so no
console window appears. Same logic as log_center.py.
"""

from __future__ import annotations

from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import log_center  # noqa: E402


if __name__ == "__main__":
    log_center.main()
