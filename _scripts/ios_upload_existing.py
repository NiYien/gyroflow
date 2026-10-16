# SPDX-License-Identifier: GPL-3.0-or-later
"""Validate and optionally sign an existing CI IPA without rebuilding it."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import subprocess
import tempfile
import zipfile

from ios_ci import check_config, upload_ipa_account, write_github_values
from ios_metadata import ROOT, _validate_build_number, load_metadata
from package_ios import output, run, sign_app, signing_settings, validate_distribution_toolchain


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_source(directory, expected_commit, root=ROOT):
    """Use the receipt's identity, but never its absolute path from another runner."""
    directory = Path(directory).resolve()
    receipt = json.loads((directory / "ios/build-receipt.json").read_text(encoding="utf-8"))
    candidates = list(directory.glob("*.ipa"))
    if len(candidates) != 1:
        raise ValueError("The selected artifact must contain exactly one IPA")
    ipa = candidates[0]
    if ipa.is_symlink() or not ipa.is_file():
        raise ValueError("The IPA must be a regular file")
    if not re.fullmatch(r"[0-9a-f]{40}", expected_commit) or receipt.get("commit") != expected_commit:
        raise ValueError("IPA receipt commit does not match the selected workflow run")
    if sha256(ipa) != receipt.get("sha256"):
        raise ValueError("IPA checksum does not match its build receipt")
    if receipt.get("signing") not in ("unsigned", "development", "distribution"):
        raise ValueError("Unknown IPA signing mode")
    if receipt.get("bundle_identifier") != load_metadata(root, {})["bundle_identifier"]:
        raise ValueError("The IPA belongs to another application")
    _validate_build_number(receipt.get("build_number"))
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", str(receipt.get("version", ""))):
        raise ValueError("Invalid IPA release version")
    with zipfile.ZipFile(ipa) as archive:
        if archive.testzip() is not None:
            raise ValueError("IPA archive integrity check failed")
        info = plistlib.loads(archive.read("Payload/NiYien.app/Info.plist"))
    for field, key in (("bundle_identifier", "CFBundleIdentifier"), ("version", "CFBundleShortVersionString"),
                       ("build_number", "CFBundleVersion")):
        if info.get(key) != receipt[field]:
            raise ValueError(f"IPA and build receipt disagree: {field}")
    return ipa, receipt


def extract_ipa(ipa, stage):
    """Extract ordinary files only and restore executable permissions from the IPA."""
    seen = set()
    with zipfile.ZipFile(ipa) as archive:
        for member in archive.infolist():
            path = PurePosixPath(member.filename)
            raw_name = member.orig_filename
            mode = member.external_attr >> 16
            normalized = str(path).casefold()
            if (path.is_absolute() or ".." in path.parts or "\\" in raw_name or ":" in raw_name
                    or any(ord(char) < 32 for char in raw_name) or normalized in seen
                    or stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR)):
                raise ValueError("Unsupported or unsafe IPA archive entry")
            if not (path.parts == ("Payload",) or path.parts[:2] == ("Payload", "NiYien.app")):
                raise ValueError("Expected only Payload/NiYien.app in the IPA")
            if any(part.endswith((".app", ".appex", ".xpc")) for part in path.parts[2:]):
                raise ValueError("Nested applications require their own signing profiles")
            seen.add(normalized)
        for member in archive.infolist():
            destination = stage / member.filename
            if member.is_dir():
                destination.mkdir(parents=True, exist_ok=True)
            else:
                destination.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(member) as source, destination.open("wb") as target:
                    shutil.copyfileobj(source, target)
                destination.chmod((member.external_attr >> 16) & 0o777 or 0o644)
    app = stage / "Payload/NiYien.app"
    info = plistlib.loads((app / "Info.plist").read_bytes())
    executable = info.get("CFBundleExecutable", "")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", executable) or executable in (".", ".."):
        raise ValueError("Invalid IPA executable name")
    platform = output("xcrun", "vtool", "-show-build", app / executable)
    if not re.search(r"^\s*platform IOS\s*$", platform, re.MULTILINE):
        raise ValueError("The IPA is not an iOS device build")
    # Validate the original build, never replace its metadata with this runner's SDK.
    validate_distribution_toolchain(info.get("DTXcode"), info.get("DTPlatformVersion"), platform)
    return app


def verify_distribution(app, metadata, stage):
    run("codesign", "--verify", "--deep", "--strict", "--verbose=2", app)
    certificate_prefix = stage / "signer-"
    run("codesign", "--display", f"--extract-certificates={certificate_prefix}", app)
    fingerprint = hashlib.sha1(Path(str(certificate_prefix) + "0").read_bytes()).hexdigest()
    _, _, granted = signing_settings("distribution", metadata,
                                    profile=app / "embedded.mobileprovision", key=fingerprint)
    actual = plistlib.loads(subprocess.check_output(["codesign", "--display", "--entitlements", ":-", str(app)]))
    for key in ("application-identifier", "com.apple.developer.team-identifier"):
        if not granted.get(key) or actual.get(key) != granted[key]:
            raise ValueError(f"IPA signing entitlement disagrees with its profile: {key}")
    if actual.get("get-task-allow"):
        raise ValueError("A development signature cannot be uploaded")


def inspect_source(directory, expected_commit, root=ROOT):
    ipa, receipt = read_source(directory, expected_commit, root)
    staging = root / "target/ios-upload"
    staging.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=staging) as temporary:
        stage = Path(temporary)
        app = extract_ipa(ipa, stage)
        if receipt["signing"] == "distribution":
            verify_distribution(app, receipt, stage)
    return {"needs_signing": str(receipt["signing"] != "distribution").lower(),
            "version": receipt["version"], "build_number": receipt["build_number"]}


def prepare_upload(directory, expected_commit, root=ROOT):
    ipa, receipt = read_source(directory, expected_commit, root)
    destination = root / "_deployment/_binaries/ios-upload"
    destination.mkdir(parents=True, exist_ok=True)
    target = destination / f"NiYien-{receipt['version']}-{receipt['build_number']}-distribution.ipa"
    if target.exists():
        raise ValueError("Upload output already exists; use a fresh workspace")
    staging = root / "target/ios-upload"
    staging.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=staging) as temporary:
        stage = Path(temporary)
        app = extract_ipa(ipa, stage)
        if receipt["signing"] != "distribution":
            sign_app(app, stage, signing_settings("distribution", receipt))
            verify_distribution(app, receipt, stage)
            run("zip", "-qr", target, "Payload", cwd=stage)
        else:
            verify_distribution(app, receipt, stage)
            shutil.copy2(ipa, target)
    updated = {**receipt, "signing": "distribution", "ipa": str(target),
               "sha256": sha256(target),
               "source_sha256": receipt["sha256"], "source_signing": receipt["signing"],
               "source_run_id": os.environ.get("SOURCE_RUN_ID", "")}
    (destination / "ios").mkdir()
    (destination / "ios/build-receipt.json").write_text(json.dumps(updated, indent=2) + "\n", encoding="utf-8")
    read_source(destination, expected_commit, root)
    return {"ipa_path": str(target), "version": receipt["version"], "build_number": receipt["build_number"]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("inspect", "check-config", "prepare", "upload-account"))
    args = parser.parse_args()
    try:
        source = ROOT / "target/ios-source"
        commit = os.environ["SOURCE_COMMIT"]
        if args.action == "inspect":
            values = inspect_source(source, commit)
        elif args.action == "check-config":
            inspected = inspect_source(source, commit)
            values = {**check_config(os.environ, require_signing=inspected["needs_signing"] == "true"), **inspected}
        elif args.action == "prepare":
            values = prepare_upload(source, commit)
        else:
            directory = ROOT / "_deployment/_binaries/ios-upload"
            if inspect_source(directory, commit)["needs_signing"] != "false":
                raise ValueError("The upload IPA must have a distribution signature")
            ipa, _ = read_source(directory, commit)
            upload_ipa_account(os.environ, str(ipa))
            return
        write_github_values(os.environ["GITHUB_OUTPUT"], values)
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        parser.exit(1, f"Existing iOS IPA upload failed: {error}\n")


if __name__ == "__main__":
    main()
