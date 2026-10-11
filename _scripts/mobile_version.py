# SPDX-License-Identifier: GPL-3.0-or-later
"""Maintain the shared Android and iOS release version."""
import argparse
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]


def version_parts(version):
    if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("Mobile release version must contain exactly three integers")
    return tuple(int(part) for part in version.split("."))


def load_version(root=ROOT):
    version = json.loads((root / "_deployment/mobile-version.json").read_text(encoding="utf-8"))["version"]
    version_parts(version)
    return version


def update_version(root=ROOT, *, version=None, bump=None):
    previous = version_parts(load_version(root))
    if bump is not None:
        index = {"major": 0, "minor": 1, "patch": 2}[bump]
        parts = list(previous)
        parts[index] += 1
        for following in range(index + 1, 3):
            parts[following] = 0
        version = ".".join(map(str, parts))
    if version_parts(version) <= previous:
        raise ValueError("The next mobile release version must be higher than the current version")
    path = root / "_deployment/mobile-version.json"
    path.write_text(json.dumps({"version": version}, indent=2) + "\n", encoding="utf-8")
    return version


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    actions = parser.add_mutually_exclusive_group()
    actions.add_argument("--set", dest="version")
    actions.add_argument("--bump", choices=("major", "minor", "patch"))
    args = parser.parse_args()
    print(update_version(version=args.version, bump=args.bump) if args.version or args.bump else load_version())
