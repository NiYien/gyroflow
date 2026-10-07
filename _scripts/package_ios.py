# SPDX-License-Identifier: GPL-3.0-or-later
"""Package an iOS build without reusing stale identity or signing metadata."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import zipfile

from ios_metadata import ROOT, load_metadata


def run(*args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def output(*args):
    return subprocess.check_output([str(arg) for arg in args], text=True).strip()


def validate_distribution_toolchain(xcode_version, sdk_version, executable_build):
    if not str(xcode_version).isdigit() or int(xcode_version) < 2600:
        raise ValueError("App Store Connect requires Xcode 26 or later")
    versions = [sdk_version] + re.findall(r"^\s*sdk\s+([0-9.]+)\s*$", executable_build, re.MULTILINE)
    if len(versions) < 2:
        raise ValueError("The executable has no verifiable iOS SDK build version")
    for version in versions:
        if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)*", str(version)) or int(str(version).split(".")[0]) < 26:
            raise ValueError("App Store Connect requires an executable built with iOS SDK 26 or later")


def signing_settings(kind, metadata):
    if kind == "unsigned":
        return None
    development = kind == "development"
    profile = Path(os.environ.get("PROVISIONING_PROFILE", ROOT / "_deployment/ios" / ("NiYien_dev.mobileprovision" if development else "NiYien_dist.mobileprovision")))
    key = os.environ.get("SIGN_KEY", "F4722C6FA73CC1749FFFDECFD1B04599513757B4" if development else "91B8D990131A6DE4525121FC14D56B39633ED71F")
    if not profile.is_file():
        raise ValueError(f"Missing provisioning profile for {metadata['bundle_identifier']}: {profile}")
    data = plistlib.loads(subprocess.check_output(["security", "cms", "-D", "-i", str(profile)]))
    entitlements = data["Entitlements"]
    team = data["TeamIdentifier"][0]
    expected = f"{team}.{metadata['bundle_identifier']}"
    if entitlements.get("application-identifier") != expected:
        raise ValueError(f"Provisioning profile must explicitly authorize {expected}")
    if bool(entitlements.get("get-task-allow")) != development:
        raise ValueError("Provisioning profile has the wrong distribution type")
    if not development and (data.get("ProvisionedDevices") or data.get("ProvisionsAllDevices")):
        raise ValueError("App Store distribution requires an App Store profile, not Ad Hoc or Enterprise")
    if data["ExpirationDate"].replace(tzinfo=timezone.utc) <= datetime.now(timezone.utc):
        raise ValueError("Provisioning profile has expired")
    certificate_hashes = {hashlib.sha1(cert).hexdigest().upper() for cert in data["DeveloperCertificates"]}
    if key.upper() not in certificate_hashes:
        raise ValueError("SIGN_KEY must be the SHA-1 of a certificate in the provisioning profile")
    return profile, key, entitlements


def make_icons(stage, app, metadata):
    assets = stage / "Images.xcassets"
    shutil.copytree(ROOT / "_deployment/ios/Resources/Images.xcassets", assets, ignore=shutil.ignore_patterns("*.png"))
    iconset = assets / "AppIcon.appiconset"
    for item in json.loads((iconset / "Contents.json").read_text())["images"]:
        if "filename" not in item:
            continue
        size = round(float(item["size"].split("x")[0]) * float(item.get("scale", "1x")[:-1]))
        run("sips", "-z", size, size, ROOT / "_deployment/ios/NiYienIcon.png", "--out", iconset / item["filename"], stdout=subprocess.DEVNULL)
    run("xcrun", "actool", assets, "--compile", app, "--platform", "iphoneos", "--minimum-deployment-target", metadata["minimum_os_version"], "--app-icon", "AppIcon", "--output-partial-info-plist", stage / "AppIcon.plist")
    return plistlib.loads((stage / "AppIcon.plist").read_bytes())


def package(profile, signing):
    metadata = load_metadata()
    signing_info = signing_settings(signing, metadata)
    target = ROOT / "target/aarch64-apple-ios"
    executable = target / profile / "gyroflow"
    if not executable.is_file():
        raise ValueError(f"iOS executable is missing: {executable}")
    platform = output("xcrun", "vtool", "-show-build", executable)
    if not re.search(r"^\s*platform IOS\s*$", platform, re.MULTILINE):
        raise ValueError("The executable is not an iOS device build")
    developer = Path(output("xcode-select", "-p"))
    xcode = plistlib.loads((developer.parent / "Info.plist").read_bytes())
    sdk_version = output("xcrun", "--sdk", "iphoneos", "--show-sdk-version")
    sdk_build = output("xcrun", "--sdk", "iphoneos", "--show-sdk-build-version")
    if signing == "distribution":
        validate_distribution_toolchain(xcode.get("DTXcode"), sdk_version, platform)

    binaries = ROOT / "_deployment/_binaries"
    stage = binaries / "ios"
    if stage.exists():
        backup = binaries / ("ios-before-" + datetime.now().strftime("%Y%m%d-%H%M%S-%f"))
        stage.rename(backup)
    app = stage / "Payload/NiYien.app"
    app.mkdir(parents=True)
    shutil.copy2(executable, app / "gyroflow")
    frameworks = target / "Frameworks"
    if not (frameworks / "mdk.framework/mdk").is_file():
        raise ValueError("The device MDK framework is missing from the build output")
    shutil.copytree(frameworks, app / "Frameworks", symlinks=True,
                    ignore=shutil.ignore_patterns("Headers", "Modules", "_CodeSignature"))
    for filename in ("PkgInfo", "PrivacyInfo.xcprivacy"):
        shutil.copy2(ROOT / "_deployment/ios" / filename, app / filename)
    for language in (ROOT / "_deployment/ios/Resources").glob("*.lproj"):
        shutil.copytree(language, app / language.name)

    info = plistlib.loads((ROOT / "_deployment/ios/Info.plist").read_bytes())
    info.update(CFBundleDisplayName=metadata["display_name"], CFBundleName=metadata["display_name"],
                CFBundleIdentifier=metadata["bundle_identifier"], CFBundleShortVersionString=metadata["version"],
                CFBundleVersion=metadata["build_number"], MinimumOSVersion=metadata["minimum_os_version"])
    info.update(BuildMachineOSBuild=output("sw_vers", "-buildVersion"), DTCompiler=xcode.get("DTCompiler", "com.apple.compilers.llvm.clang.1_0"),
                DTPlatformBuild=sdk_build, DTPlatformName="iphoneos", DTPlatformVersion=sdk_version,
                DTSDKBuild=sdk_build, DTSDKName="iphoneos" + sdk_version,
                DTXcode=xcode["DTXcode"], DTXcodeBuild=xcode["DTXcodeBuild"])
    info.update(make_icons(stage, app, metadata))
    (app / "Info.plist").write_bytes(plistlib.dumps(info, sort_keys=False))
    run("xcrun", "ibtool", "--errors", "--warnings", "--notices", "--module", "gyroflow", "--target-device", "iphone", "--target-device", "ipad",
        "--minimum-deployment-target", metadata["minimum_os_version"], "--compilation-directory", stage, ROOT / "_deployment/ios/LaunchScreen.storyboard")
    run("xcrun", "ibtool", "--link", app, stage / "LaunchScreen.storyboardc")
    run("xcrun", "dsymutil", executable, "-o", stage / "NiYien.app.dSYM")

    if signing_info:
        provision, key, granted = signing_info
        shutil.copy2(provision, app / "embedded.mobileprovision")
        keys = ("application-identifier", "com.apple.developer.team-identifier", "get-task-allow", "beta-reports-active", "com.apple.developer.kernel.increased-memory-limit")
        entitlements = {key: granted[key] for key in keys if key in granted}
        entitlements["keychain-access-groups"] = [granted["application-identifier"]]
        entitlement_path = stage / "entitlements.xcent"
        entitlement_path.write_bytes(plistlib.dumps(entitlements))
        for framework in sorted((app / "Frameworks").glob("*.framework")):
            run("codesign", "--force", "--sign", key, framework)
        run("codesign", "--force", "--generate-entitlement-der", "--sign", key, "--entitlements", entitlement_path, app)
        run("codesign", "--verify", "--deep", "--strict", "--verbose=2", app)
    else:
        print("Unsigned package: a matching provisioning profile is required before installation or upload.", flush=True)

    ipa = binaries / f"NiYien-{metadata['version']}-{metadata['build_number']}-{signing}.ipa"
    if ipa.exists():
        ipa.rename(ipa.with_name(ipa.stem + "-" + datetime.now().strftime("%Y%m%d-%H%M%S-%f") + ".ipa"))
    run("zip", "-qr", ipa, "Payload", cwd=stage)
    with zipfile.ZipFile(ipa) as archive:
        if archive.testzip() is not None:
            raise ValueError("IPA archive integrity check failed")
    receipt = {**metadata, "signing": signing, "profile": profile, "ipa": str(ipa),
               "sha256": hashlib.sha256(ipa.read_bytes()).hexdigest(), "commit": output("git", "rev-parse", "HEAD")}
    receipt["source_dirty"] = bool(output("git", "status", "--porcelain"))
    (stage / "build-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", default="deploy")
    parser.add_argument("--signing", choices=("development", "distribution", "unsigned"), default="development")
    args = parser.parse_args()
    package(args.profile, args.signing)
