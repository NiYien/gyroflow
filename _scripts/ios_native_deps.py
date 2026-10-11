# SPDX-License-Identifier: GPL-3.0-or-later
"""Prepare iOS SDKs and package runtime frameworks independently of Cargo caches."""
import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MDK_URL = "https://github.com/wang-bin/mdk-sdk/releases/download/v0.38.0/mdk-sdk-iOS.tar.xz"
MDK_SHA256 = "8df01971556dfca0a0bf2434f4c7012d3ae6f3263041faa28d3d30df27c5cf3e"
BRAW_ARCHIVE = "Blackmagic_RAW_SDK_iOS_5.0.0.tar.gz"
BRAW_SHA256 = "79c3746843fa8d6ffe34a99de6267307e3170f43acaa6a968c6979d2a1224c2b"
BRAW_FRAMEWORKS = ("BlackmagicRawAPI", "DecoderMetal")


def download(url, destination):
    subprocess.run(["curl", "--fail", "--location", "--retry", "3", "--connect-timeout", "30",
                    "--max-time", "600", url, "--output", str(destination)], check=True)


def install_archive(url, digest, destination, subdirectory="", downloader=download):
    """Verify the entire download before extracting into a fresh directory."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".ios-sdk-", dir=destination.parent) as temporary:
        temporary = Path(temporary)
        archive = temporary / "download.tar"
        downloader(url, archive)
        if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
            raise ValueError(f"SDK archive checksum mismatch: {url}")
        extracted = temporary / "extracted"
        extracted.mkdir()
        with tarfile.open(archive) as bundle:
            # These pinned archives only need ordinary files and directories.
            for member in bundle.getmembers():
                path = Path(member.name)
                if path.is_absolute() or ".." in path.parts or not (member.isfile() or member.isdir()):
                    raise ValueError(f"Unsupported SDK archive entry: {member.name}")
            bundle.extractall(extracted)
        (extracted / subdirectory).rename(destination)


def mdk_directory(root=ROOT, environment=None):
    environment = os.environ if environment is None else environment
    return Path(environment.get("MDK_SDK") or root / "ext/mdk-sdk-ios").resolve()


def mdk_framework(sdk):
    framework = sdk / "lib/mdk.xcframework/ios-arm64/mdk.framework"
    if not framework.is_dir():
        framework = sdk / "lib/mdk.framework"
    if not (framework / "mdk").is_file() or not (framework / "Info.plist").is_file():
        raise ValueError(f"Missing iOS device MDK framework in {sdk}; run just ios install-deps")
    info = plistlib.loads((framework / "Info.plist").read_bytes())
    if info.get("CFBundleVersion") != "0.38.0":
        raise ValueError(f"Expected the validated MDK 0.38.0 SDK in {sdk}")
    return framework


def prepare_mdk(sdk):
    framework = mdk_framework(sdk)
    headers = sdk / "include/mdk"
    if not (headers / "Player.h").is_file() or not (headers / "c/Player.h").is_file():
        shutil.copytree(framework / "Headers", headers, dirs_exist_ok=True)
    # qml-video-rs checks this legacy path before accepting MDK_SDK.
    legacy = sdk / "lib/mdk.framework"
    if not legacy.exists():
        legacy.symlink_to("mdk.xcframework/ios-arm64/mdk.framework", target_is_directory=True)
    if not (legacy / "mdk").is_file():
        raise ValueError(f"MDK_SDK is not compatible with qml-video-rs: {sdk}")
    return sdk


def braw_directory(root=ROOT):
    return root / "ext/braw-sdk-ios-5.0.0"


def runtime_frameworks(root=ROOT, environment=None):
    frameworks = [mdk_framework(mdk_directory(root, environment))]
    frameworks.extend(braw_directory(root) / (name + ".framework") for name in BRAW_FRAMEWORKS)
    for framework in frameworks:
        if not (framework / framework.stem).is_file() or not (framework / "Info.plist").is_file():
            raise ValueError(f"Missing runtime framework {framework}; run just ios install-deps")
    return frameworks


def install(root=ROOT, environment=None, downloader=download):
    environment = os.environ if environment is None else environment
    sdk = mdk_directory(root, environment)
    if not sdk.exists():
        if environment.get("MDK_SDK"):
            raise ValueError(f"Explicit MDK_SDK does not exist: {sdk}")
        install_archive(MDK_URL, MDK_SHA256, sdk, "mdk-sdk", downloader)
    # Keep existing local SDKs; CI installs the pinned archive on a cache miss.
    prepare_mdk(sdk)
    braw = braw_directory(root)
    if not braw.exists():
        base = environment.get("SDK_BASE") or "https://www.niyien.com/api/sdk"
        install_archive(base.rstrip("/") + "/" + BRAW_ARCHIVE, BRAW_SHA256, braw, downloader=downloader)
    runtime_frameworks(root, environment)
    return sdk


def copy_runtime_frameworks(destination, root=ROOT, environment=None):
    """Always copy from ext, even when Cargo restores every dependency from cache."""
    frameworks = runtime_frameworks(root, environment)
    destination.mkdir(parents=True, exist_ok=False)
    for framework in frameworks:
        shutil.copytree(framework, destination / framework.name,
                        ignore=shutil.ignore_patterns("Headers", "Modules", "_CodeSignature"))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("install", "mdk-path"))
    args = parser.parse_args()
    try:
        sdk = install() if args.action == "install" else prepare_mdk(mdk_directory())
        # qml-video-rs also concatenates paths without inserting a separator.
        print(str(sdk) + "/")
    except (ValueError, OSError, tarfile.TarError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"iOS native dependencies: {error}\n")
