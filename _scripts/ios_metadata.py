# SPDX-License-Identifier: GPL-3.0-or-later
"""Read and validate the iOS identity independently of desktop releases."""
import argparse
import json
import os
from pathlib import Path
import re
if __package__:
    from .mobile_version import load_version
else:
    from mobile_version import load_version

ROOT = Path(__file__).resolve().parents[1]


def _build_state(root):
    path = root / "target/ios-version/build-number.json"
    if not path.exists():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def _validate_build_number(value):
    if not isinstance(value, str) or not re.fullmatch(r"[1-9][0-9]{0,3}(?:\.[0-9]{1,2}){0,2}", value):
        raise ValueError("iOS build number must use one to three numeric parts (4/2/2 digits)")
    return value


def _build_parts(value):
    parts = tuple(int(part) for part in _validate_build_number(value).split("."))
    return parts + (0,) * (3 - len(parts))


def _next_build_number(value):
    parts = [int(part) for part in value.split(".")]
    parts[-1] += 1
    for index in range(len(parts) - 1, 0, -1):
        if parts[index] > 99:
            parts[index] = 0
            parts[index - 1] += 1
    return _validate_build_number(".".join(map(str, parts)))


def _ci_build_number(environment, floor):
    run = environment.get("GITHUB_RUN_NUMBER", "")
    attempt = environment.get("GITHUB_RUN_ATTEMPT", "1")
    if not re.fullmatch(r"[1-9][0-9]*", run) or not re.fullmatch(r"[1-9][0-9]?", attempt):
        raise ValueError("iOS CI needs a positive GITHUB_RUN_NUMBER and GITHUB_RUN_ATTEMPT from 1 to 99")
    return _validate_build_number(f"{_build_parts(floor)[0] + int(run)}.{int(attempt)}")


def load_metadata(root=ROOT, environment=None):
    environment = os.environ if environment is None else environment
    metadata = json.loads((root / "_deployment/ios/app.json").read_text(encoding="utf-8"))
    metadata["version"] = load_version(root)
    state = _build_state(root)
    build = _validate_build_number(metadata["build_number"])
    if state.get("version") == metadata["version"]:
        build = max((build, _validate_build_number(state.get("build_number"))), key=_build_parts)
    metadata["build_number"] = _validate_build_number(environment.get("NIYIEN_IOS_BUILD_NUMBER", build))
    if not re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+){2,}", metadata["bundle_identifier"]):
        raise ValueError("Invalid iOS bundle identifier")
    if not metadata["display_name"].strip():
        raise ValueError("iOS display name is empty")
    return metadata


def allocate_build(root=ROOT, environment=None):
    """Reserve one build number before compilation and reuse it during packaging."""
    environment = os.environ if environment is None else environment
    path = root / "target/ios-version/build-number.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    lock = path.with_suffix(".lock")
    try:
        descriptor = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    except FileExistsError as error:
        raise ValueError(f"Another iOS build is allocating a number: {lock}") from error
    os.close(descriptor)
    try:
        metadata = load_metadata(root, {})
        if "NIYIEN_IOS_BUILD_NUMBER" in environment:
            number = environment["NIYIEN_IOS_BUILD_NUMBER"]
        elif environment.get("GITHUB_ACTIONS") == "true":
            settings = json.loads((root / "_deployment/ios/app.json").read_text(encoding="utf-8"))
            number = _ci_build_number(environment, settings["build_number"])
        else:
            number = _next_build_number(metadata["build_number"])
        _validate_build_number(number)
        if _build_parts(number) <= _build_parts(metadata["build_number"]):
            raise ValueError("The next iOS build number must be higher than the last reserved number")
        metadata["build_number"] = number
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps({"version": metadata["version"], "build_number": number}) + "\n", encoding="utf-8")
        temporary.replace(path)
        return metadata
    finally:
        lock.unlink()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("field", nargs="?", default="json")
    parser.add_argument("--allocate-build", action="store_true")
    args = parser.parse_args()
    data = allocate_build() if args.allocate_build else load_metadata()
    print(json.dumps(data) if args.field == "json" else data[args.field])
