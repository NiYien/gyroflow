# SPDX-License-Identifier: GPL-3.0-or-later
"""Maintain the shared Android and iOS release version."""
import argparse
import json
import os
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
    settings = json.loads(path.read_text(encoding="utf-8"))
    settings["version"] = version
    path.write_text(json.dumps(settings, indent=2) + "\n", encoding="utf-8")
    return version


def prepare_ci_version(root=ROOT, environment=None):
    """Materialize the same release version before either mobile build starts."""
    environment = os.environ if environment is None else environment
    run = environment.get("GITHUB_RUN_NUMBER", "")
    if environment.get("GITHUB_ACTIONS") != "true" or not re.fullmatch(r"[1-9][0-9]*", run):
        raise ValueError("Mobile CI version needs GitHub Actions and a positive GITHUB_RUN_NUMBER")
    path = root / "_deployment/mobile-version.json"
    settings = json.loads(path.read_text(encoding="utf-8"))
    parts = list(version_parts(settings["version"]))
    previous_run = settings.get("ci_run_number")
    if type(previous_run) is not int or previous_run < 1:
        raise ValueError("Mobile version configuration needs a positive ci_run_number baseline")
    run = int(run)
    if run < previous_run:
        raise ValueError("GitHub run number is older than the mobile version baseline")
    parts[2] += run - previous_run
    settings["version"] = ".".join(map(str, parts))
    settings["ci_run_number"] = run
    path.write_text(json.dumps(settings, indent=2) + "\n", encoding="utf-8")
    return settings["version"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    actions = parser.add_mutually_exclusive_group()
    actions.add_argument("--set", dest="version")
    actions.add_argument("--bump", choices=("major", "minor", "patch"))
    actions.add_argument("--prepare-ci", action="store_true")
    args = parser.parse_args()
    if args.prepare_ci:
        print(prepare_ci_version())
    else:
        print(update_version(version=args.version, bump=args.bump) if args.version or args.bump else load_version())
