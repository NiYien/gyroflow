#!/usr/bin/env python3
import json
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib


ROOT = Path(__file__).resolve().parent.parent
CONFIG_PATH = ROOT / "distribution" / "niyien.toml"


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: read_distribution_config.py dotted.path", file=sys.stderr)
        return 1
    with CONFIG_PATH.open("rb") as fh:
        data = tomllib.load(fh)

    value = data
    for part in sys.argv[1].split("."):
        value = value[part]

    if isinstance(value, (dict, list)):
        print(json.dumps(value))
    else:
        print(value)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
