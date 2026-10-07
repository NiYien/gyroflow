# SPDX-License-Identifier: GPL-3.0-or-later
"""Package the original review sample and its reproducible source."""
import argparse
import hashlib
import json
from pathlib import Path
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def prepare(output):
    inputs = {path.name: path for path in (ROOT / "resources/demo").iterdir()
              if path.name in {"niyien-demo.mp4", "niyien-demo.gcsv", "niyien-demo.gyroflow", "NOTICE.txt", "REVIEW.txt"}}
    if len(inputs) != 5:
        raise ValueError("The review sample is incomplete")
    inputs.update({"LICENSE": ROOT / "LICENSE", "generate_mobile_demo.py": ROOT / "_scripts/generate_mobile_demo.py"})
    output.parent.mkdir(parents=True, exist_ok=True)
    hashes = {}
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, source in sorted(inputs.items()):
            data = source.read_bytes()
            hashes[name] = hashlib.sha256(data).hexdigest()
            entry = zipfile.ZipInfo("NiYien-review-sample/" + name, (2026, 10, 7, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            entry.create_system = 3
            entry.external_attr = 0o100644 << 16
            archive.writestr(entry, data)
    with zipfile.ZipFile(output) as archive:
        if archive.testzip() is not None:
            raise ValueError("The review sample ZIP failed its integrity check")
    return {"archive": str(output), "sha256": hashlib.sha256(output.read_bytes()).hexdigest(), "files": hashes}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=ROOT / "_deployment/_binaries/NiYien-review-sample.zip")
    args = parser.parse_args()
    print(json.dumps(prepare(args.output), indent=2))
