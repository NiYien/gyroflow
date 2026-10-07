# SPDX-License-Identifier: GPL-3.0-or-later
"""Collect notices from the locked iOS dependency graph without local paths."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def collect(metadata):
    index_file = ROOT / "resources/legal/notice-sources.json"
    recovered = json.loads(index_file.read_text(encoding="utf-8")) if index_file.exists() else {}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    selected = set()

    def visit(package):
        if package in selected:
            return
        selected.add(package)
        for dependency in nodes.get(package, {}).get("deps", []):
            if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"]):
                visit(dependency["pkg"])

    visit(metadata["resolve"]["root"])
    entries, notices, missing = [], {}, []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["id"] not in selected:
            continue
        folder = Path(package["manifest_path"]).parent
        files = [path for path in folder.iterdir() if path.is_file() and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT"))]
        if package.get("license_file"):
            files.append(folder / package["license_file"])
        files.extend(ROOT / "resources/legal" / item["file"]
                     for item in recovered.get(f"{package['name']} {package['version']}", []))
        if folder.is_relative_to(ROOT) and not files:
            files.append(ROOT / "LICENSE")
        expressions = package.get("license") or "See accompanying license text"
        notice_origin = "package or upstream notice"
        if not files:
            # Preserve declared authors and provide standard terms when upstream omits a notice file.
            standards = [ROOT / "resources/legal/native" / (name + ".txt")
                         for name in re.split(r"\s+OR\s+|\s+AND\s+|/", expressions)]
            files.extend(path for path in standards if path.is_file())
            notice_origin = "standard license text; upstream package has no notice file"
        hashes = []
        for path in sorted(set(files)):
            if not path.is_file():
                continue
            text = path.read_text(encoding="utf-8", errors="replace").strip()
            if not text:
                continue
            digest = hashlib.sha256(text.encode()).hexdigest()
            hashes.append(digest)
            if digest not in notices:
                notices[digest] = {"packages": [], "text": text}
            notices[digest]["packages"].append(f"{package['name']} {package['version']}")
        if not hashes:
            missing.append(f"{package['name']} {package['version']}")
        source = package.get("source") or "https://github.com/NiYien/gyroflow"
        entries.append({"name": package["name"], "version": package["version"], "license": expressions,
                        "source": source, "repository": package.get("repository"), "authors": package.get("authors", []),
                        "notice_origin": notice_origin, "notice_hashes": hashes})
    return entries, notices, missing


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--metadata")
    args = parser.parse_args()
    raw = Path(args.metadata).read_bytes() if args.metadata else subprocess.check_output(
        ["cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", "aarch64-apple-ios"], cwd=ROOT)
    entries, notices, missing = collect(json.loads(raw))
    legal = ROOT / "resources/legal"
    legal.mkdir(parents=True, exist_ok=True)
    parts = ["NiYien — open-source notices\n\nNiYien is a modified version of Gyroflow, distributed under GPLv3 with the upstream additional permissions.\nSource: https://github.com/NiYien/gyroflow\nUpstream: https://github.com/gyroflow/gyroflow\n\nThe following inventory covers locked iOS runtime and build dependencies; build tools are included for reproducibility. Native SDK terms are listed separately.\n"]
    parts.append("\n\n".join(f"{p['name']} {p['version']}: {p['license']}\n"
                            + "Authors: " + (", ".join(p['authors']) or "See source repository") + "\n"
                            + str(p['repository'] or p['source']) + "\n" + p['notice_origin'] for p in entries))
    for notice in notices.values():
        parts.append("\n\n" + "=" * 64 + "\n" + ", ".join(notice["packages"]) + "\n\n" + notice["text"])
    for path in sorted((legal / "native").glob("*.txt"), key=lambda path: path.name):
        parts.append("\n\n" + "=" * 64 + "\n" + path.stem + "\n\n" + path.read_text(encoding="utf-8"))
    (legal / "mobile-licenses.txt").write_text("\n".join(parts) + "\n", encoding="utf-8", newline="\n")
    (legal / "ios-dependencies.json").write_text(json.dumps(entries, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({"packages": len(entries), "unique_notices": len(notices), "missing_notice_files": missing}, indent=2))


if __name__ == "__main__":
    main()
