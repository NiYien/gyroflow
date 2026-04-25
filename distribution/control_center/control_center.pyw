#!/usr/bin/env python3
"""GUI entry — double-click launches via pythonw.exe (no console window).

Use this for desktop shortcuts. Command-line users should keep using
`python control_center.py` (which writes diagnostic output to stderr).
"""

from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))

from control_center import main  # noqa: E402

if __name__ == "__main__":
    main()
