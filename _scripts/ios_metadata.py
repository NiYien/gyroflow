# SPDX-License-Identifier: GPL-3.0-or-later
"""Read and validate the iOS identity independently of desktop releases."""
import argparse
import json
import os
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]


def load_metadata(root=ROOT, environment=None):
    environment = os.environ if environment is None else environment
    metadata = json.loads((root / "_deployment/ios/app.json").read_text())
    metadata["build_number"] = environment.get("NIYIEN_IOS_BUILD_NUMBER", metadata["build_number"])
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", metadata["version"]):
        raise ValueError("iOS version must contain exactly three integers")
    if not re.fullmatch(r"[1-9][0-9]{0,3}", metadata["build_number"]):
        raise ValueError("iOS build number must be an integer from 1 to 9999")
    if not re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+){2,}", metadata["bundle_identifier"]):
        raise ValueError("Invalid iOS bundle identifier")
    if not metadata["display_name"].strip():
        raise ValueError("iOS display name is empty")
    return metadata


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("field", nargs="?", default="json")
    args = parser.parse_args()
    data = load_metadata()
    print(json.dumps(data) if args.field == "json" else data[args.field])
