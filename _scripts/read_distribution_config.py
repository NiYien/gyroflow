#!/usr/bin/env python3
import json
import sys
from pathlib import Path

if __package__:
    from .bootstrap_toml import dotted_value
else:
    from bootstrap_toml import dotted_value


ROOT = Path(__file__).resolve().parent.parent
CONFIG_PATH = ROOT / "distribution" / "niyien.toml"


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: read_distribution_config.py dotted.path", file=sys.stderr)
        return 1
    value = dotted_value(CONFIG_PATH, sys.argv[1])

    if isinstance(value, (dict, list)):
        print(json.dumps(value))
    else:
        print(value)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
