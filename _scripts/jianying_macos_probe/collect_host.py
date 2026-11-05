# SPDX-License-Identifier: GPL-3.0-or-later
"""Collect evidence from an explicitly selected macOS app without altering it."""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import platform
import subprocess
import sys


def command(*args):
    result = subprocess.run(args, capture_output=True, text=True, errors="replace")
    return {"exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("Run this collector on macOS")
    app = args.app.resolve(strict=True)
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    executable = app / "Contents/MacOS" / info["CFBundleExecutable"]
    report = {
        "scope": "Static evidence only; strings do not prove third-party OFX loading",
        "system": {"macos": platform.mac_ver()[0], "machine": platform.machine()},
        "app": str(app),
        "identity": {key: info.get(key) for key in ("CFBundleIdentifier", "CFBundleShortVersionString", "CFBundleVersion")},
        "signature": command("codesign", "-dvvv", str(app)),
        "entitlements": command("codesign", "-d", "--entitlements", ":-", str(app)),
        "binaries": [],
    }
    candidates = {executable}
    frameworks = app / "Contents/Frameworks"
    if frameworks.is_dir():
        candidates.update(path for path in frameworks.rglob("*") if path.is_file()
                          and any(name in path.name.lower() for name in ("cccreator", "vecreator")))
    for binary in sorted(candidates):
        data = binary.read_bytes()
        matches = [token for token in (b"OfxGetPlugin", b"OFX_PLUGIN_PATH", b".ofx.bundle", b"OfxImageEffectPropMetal", b"OfxImageEffectPropOpenGL", b"OfxImageEffectPropOpenCL") if token in data]
        report["binaries"].append({"path": str(binary), "sha256": hashlib.sha256(data).hexdigest(),
            "architectures": command("lipo", "-archs", str(binary)), "dependencies": command("otool", "-L", str(binary)),
            "loader_strings": [token.decode() for token in matches]})
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(args.output.resolve())


if __name__ == "__main__":
    main()
