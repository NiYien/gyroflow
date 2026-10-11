# SPDX-License-Identifier: GPL-3.0-or-later
"""Prepare signing and verify the exact IPA uploaded by the iOS workflow."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import uuid
import zipfile

from ios_metadata import ROOT, load_metadata
from package_ios import signing_settings

API_CONFIGURATION = {
    "APPSTORE_ISSUER_ID": "MACOS_ITCONNECT_ISSUER",
    "APPSTORE_API_KEY_ID": "MACOS_ITCONNECT_KEY_ID",
    "APPSTORE_API_PRIVATE_KEY": "MACOS_ITCONNECT_KEY",
}
UPLOAD_TAG_TRAILER = "NiYien-IOS-Upload-To-Connect"


def parse_boolean(value):
    if isinstance(value, bool):
        return value
    if value not in ("true", "false"):
        raise ValueError("Upload selection must be true or false")
    return value == "true"


def upload_requested(environment, root=ROOT):
    reference = environment.get("GITHUB_REF", "")
    if environment.get("GITHUB_EVENT_NAME") == "push" and reference.startswith("refs/tags/"):
        if not re.fullmatch(r"refs/tags/v[0-9]+\.[0-9]+\.[0-9]+", reference):
            raise ValueError("Release tags must use v<major>.<minor>.<patch>")
        kind = subprocess.check_output(["git", "cat-file", "-t", reference], cwd=root, text=True).strip()
        if kind != "tag":
            return False
        annotation = subprocess.check_output(["git", "for-each-ref", "--format=%(contents)", reference], cwd=root, text=True)
        values = re.findall(rf"^{UPLOAD_TAG_TRAILER}: (true|false)$", annotation, re.MULTILINE)
        if len(values) > 1:
            raise ValueError("Tag contains multiple iOS upload selections")
        return values == ["true"]
    return parse_boolean(environment.get("IOS_UPLOAD_TO_CONNECT", "false"))


def check_config(environment, root=ROOT):
    reference = environment.get("GITHUB_REF", "")
    if reference.startswith("refs/tags/") and not re.fullmatch(r"refs/tags/v[0-9]+\.[0-9]+\.[0-9]+", reference):
        raise ValueError("Release tags must use v<major>.<minor>.<patch>")
    metadata = load_metadata(root, {})
    if not upload_requested(environment, root):
        return {"bundle_identifier": metadata["bundle_identifier"], "upload_enabled": "false",
                "upload_mode": "none", "profile_mode": "none"}
    present = lambda name: bool(environment.get(name, "").strip())
    missing = []
    for primary, fallback in (("IOS_CERTIFICATES", "MACOS_CERTIFICATES"), ("IOS_CERTIFICATE_PWD", "MACOS_CERTIFICATE_PWD")):
        if not present(primary) and not present(fallback):
            missing.append(f"{fallback} (or {primary})")
    has_account = present("MACOS_ACCOUNT_USER") and present("MACOS_ACCOUNT_PASS")
    missing_api = [name for name, alias in API_CONFIGURATION.items() if not present(name) and not present(alias)]
    has_api = not missing_api
    if not has_account and not has_api:
        missing.append("MACOS_ACCOUNT_USER + MACOS_ACCOUNT_PASS, or " + ", ".join(missing_api))
    if not has_api and not present("IOS_PROVISIONING_PROFILE"):
        missing.append("IOS_PROVISIONING_PROFILE (or complete App Store Connect API credentials)")
    if missing:
        raise ValueError("Configure GitHub Actions secrets/variables: " + ", ".join(missing))
    fingerprint = environment.get("IOS_SIGNING_FINGERPRINT", "").strip()
    if fingerprint and not re.fullmatch(r"[0-9a-fA-F]{40}", fingerprint):
        raise ValueError("IOS_SIGNING_FINGERPRINT must be a certificate SHA-1 fingerprint")
    return {"bundle_identifier": metadata["bundle_identifier"], "upload_enabled": "true",
            "upload_mode": "account" if has_account else "api", "profile_mode": "api" if has_api else "embedded"}


def distribution_identities():
    response = subprocess.check_output(["security", "find-identity", "-v", "-p", "codesigning"], text=True)
    return list(dict.fromkeys(re.findall(r'\b([0-9A-Fa-f]{40})\s+"(?:Apple Distribution|iPhone Distribution):[^\"]*"', response)))


def select_profile(environment, root=ROOT, directory=None, validator=signing_settings, identities=None):
    if environment.get("IOS_PROFILE_MODE") == "embedded":
        value = environment.get("IOS_PROVISIONING_PROFILE", "")
        if not value.strip():
            raise ValueError("IOS_PROVISIONING_PROFILE is required without API provisioning")
        try:
            data = base64.b64decode("".join(value.split()), validate=True)
        except ValueError as error:
            raise ValueError("IOS_PROVISIONING_PROFILE must be Base64 encoded") from error
        path = Path(environment.get("RUNNER_TEMP", root / "target/ios-signing")) / "NiYien-dist.mobileprovision"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        candidates = [path]
    else:
        profiles = json.loads(environment.get("IOS_PROVISIONING_PROFILES", "[]"))
        if not isinstance(profiles, list) or not profiles:
            raise ValueError("No IOS_APP_STORE provisioning profiles were downloaded")
        directory = directory or Path.home() / "Library/MobileDevice/Provisioning Profiles"
        candidates = []
        for profile in profiles:
            identifier = profile["udid"]
            if str(uuid.UUID(identifier)) != identifier.lower():
                raise ValueError("Provisioning profile identifier must be a UUID")
            candidates.append(directory / (identifier + ".mobileprovision"))
    identities = distribution_identities() if identities is None else identities
    fingerprint = environment.get("IOS_SIGNING_FINGERPRINT", "").strip()
    keys = [key for key in identities if not fingerprint or key.upper() == fingerprint.upper()]
    if not keys:
        raise ValueError("Imported P12 has no matching Apple Distribution private key; include it in MACOS_CERTIFICATES or set IOS_CERTIFICATES")
    metadata = load_metadata(root, {})
    previous = {name: os.environ.get(name) for name in ("PROVISIONING_PROFILE", "SIGN_KEY")}
    errors = []
    try:
        for path in candidates:
            if not path.is_file():
                errors.append(f"Profile file is missing: {path.name}")
                continue
            os.environ["PROVISIONING_PROFILE"] = str(path)
            for identity in keys:
                os.environ["SIGN_KEY"] = identity
                try:
                    selected, key, _ = validator("distribution", metadata)
                except (ValueError, subprocess.CalledProcessError) as error:
                    errors.append(str(error))
                    continue
                return {"PROVISIONING_PROFILE": str(selected), "SIGN_KEY": key}
    finally:
        for name, value in previous.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
    raise ValueError("No profile matches the bundle and signing certificate: " + "; ".join(errors))


def prepare_artifact(root=ROOT, require_distribution=True):
    binaries = (root / "_deployment/_binaries").resolve()
    receipt = json.loads((binaries / "ios/build-receipt.json").read_text(encoding="utf-8"))
    ipa = Path(receipt["ipa"]).resolve()
    if not ipa.is_relative_to(binaries) or not ipa.is_file() or ipa.suffix != ".ipa":
        raise ValueError("The build receipt must reference an IPA inside _deployment/_binaries")
    if require_distribution and receipt.get("signing") != "distribution":
        raise ValueError("App Store Connect requires a distribution-signed IPA")
    if receipt.get("signing") not in ("distribution", "development", "unsigned"):
        raise ValueError("Unknown IPA signing mode")
    if hashlib.sha256(ipa.read_bytes()).hexdigest() != receipt.get("sha256"):
        raise ValueError("IPA checksum does not match its build receipt")
    metadata = load_metadata(root, {})
    with zipfile.ZipFile(ipa) as archive:
        if archive.testzip() is not None:
            raise ValueError("IPA archive integrity check failed")
        info = plistlib.loads(archive.read("Payload/NiYien.app/Info.plist"))
    for field, key in (("bundle_identifier", "CFBundleIdentifier"), ("version", "CFBundleShortVersionString"),
                       ("build_number", "CFBundleVersion")):
        if info.get(key) != metadata[field] or receipt.get(field) != metadata[field]:
            raise ValueError(f"IPA, receipt and build configuration disagree: {field}")
    return {"ipa_path": str(ipa)}


def write_github_values(filename, values):
    with Path(filename).open("a", encoding="utf-8") as file:
        for key, value in values.items():
            if "\n" in value or "\r" in value:
                raise ValueError(f"GitHub value cannot contain a newline: {key}")
            file.write(f"{key}={value}\n")


def upload_account(environment, root=ROOT, runner=subprocess.run):
    if not upload_requested(environment, root):
        raise ValueError("App Store Connect upload is disabled for this run")
    if not environment.get("MACOS_ACCOUNT_USER", "").strip() or not environment.get("MACOS_ACCOUNT_PASS", "").strip():
        raise ValueError("MACOS_ACCOUNT_USER and MACOS_ACCOUNT_PASS are required for account upload")
    ipa = prepare_artifact(root)["ipa_path"]
    result = runner(["xcrun", "altool", "--upload-app", "--file", ipa, "--type", "ios",
                     "--username", environment["MACOS_ACCOUNT_USER"], "--password", "@env:MACOS_ACCOUNT_PASS"],
                    env={**os.environ, **environment}, check=False)
    if result.returncode:
        raise ValueError(f"App Store Connect upload failed with exit code {result.returncode}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("check-config", "prepare-signing", "prepare-artifact", "upload-account"))
    args = parser.parse_args()
    try:
        if args.action == "check-config":
            write_github_values(os.environ["GITHUB_OUTPUT"], check_config(os.environ))
        elif args.action == "prepare-signing":
            write_github_values(os.environ["GITHUB_ENV"], select_profile(os.environ))
        elif args.action == "prepare-artifact":
            required = parse_boolean(os.environ.get("IOS_REQUIRE_DISTRIBUTION", "true"))
            write_github_values(os.environ["GITHUB_OUTPUT"], prepare_artifact(require_distribution=required))
        else:
            upload_account(os.environ)
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, f"iOS CI setup failed: {error}\n")
