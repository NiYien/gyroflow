# SPDX-License-Identifier: GPL-3.0-or-later
"""Install the pinned MDK runtimes used by the preview and RAW decoders."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = Path(__file__).with_name('mdk_sdk_manifest.json')
DEFAULT_SDK_BASE = 'https://www.niyien.com/api/sdk'


def manifest():
    return json.loads(MANIFEST.read_text())


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def validate(sdk, package):
    for relative, expected in package['runtime_sha256'].items():
        path = sdk / relative
        if not path.is_file() or sha256(path) != expected:
            raise ValueError(f'MDK SDK does not match the runtime pin: {path}. Unset MDK_SDK to install the pinned snapshot.')


def prepare_headers(sdk, target):
    if target != 'ios':
        return
    framework = sdk / 'lib/mdk.xcframework/ios-arm64/mdk.framework'
    headers = sdk / 'include/mdk'
    if not (headers / 'Player.h').is_file():
        shutil.copytree(framework / 'Headers', headers, dirs_exist_ok=True)
    legacy = sdk / 'lib/mdk.framework'
    if not legacy.exists():
        legacy.symlink_to('mdk.xcframework/ios-arm64/mdk.framework', target_is_directory=True)


def download(url, destination):
    subprocess.run(['curl', '--fail', '--location', '--retry', '3', '--connect-timeout', '30',
                    '--max-time', '600', '--output', str(destination), url], check=True)


def extract(archive, destination):
    if archive.name.endswith('.7z'):
        subprocess.run(['7z', 'x', '-y', '-o' + str(destination), str(archive)], check=True,
                       stdout=subprocess.DEVNULL)
        return
    with tarfile.open(archive) as bundle:
        for member in bundle.getmembers():
            path = Path(member.name)
            if path.is_absolute() or '..' in path.parts or not (
                    member.isfile() or member.isdir() or member.issym() or member.islnk()):
                raise ValueError(f'Unsupported MDK archive entry: {member.name}')
            if member.issym() or member.islnk():
                base = destination / path.parent if member.issym() else destination
                link = base / member.linkname
                if Path(member.linkname).is_absolute() or not link.resolve().is_relative_to(destination.resolve()):
                    raise ValueError(f'Unsafe MDK archive link: {member.name}')
        bundle.extractall(destination)


def install(target, root=ROOT, environment=None, downloader=download):
    environment = os.environ if environment is None else environment
    data = manifest()
    package = data['platforms'][target]
    override = environment.get('MDK_SDK', '').strip()
    if override:
        sdk = Path(override).resolve()
        validate(sdk, package)
        prepare_headers(sdk, target)
        return sdk
    sdk = root / 'ext' / ('mdk-' + data['revision']) / target / 'mdk-sdk'
    if sdk.exists():
        try:
            validate(sdk, package)
            prepare_headers(sdk, target)
            return sdk.resolve()
        except ValueError:
            pass
    sdk.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.mdk-', dir=sdk.parent) as temporary:
        temporary = Path(temporary)
        archive = temporary / package['archive']
        base = environment.get('SDK_BASE') or DEFAULT_SDK_BASE
        url = base.rstrip('/') + '/' + package['archive']
        if not environment.get('SDK_BASE'):
            url = package.get('url', url)
        downloader(url, archive)
        if sha256(archive) != package['sha256']:
            raise ValueError(f'MDK archive checksum mismatch: {package["archive"]}')
        extracted = temporary / 'extracted'
        extracted.mkdir()
        extract(archive, extracted)
        candidate = extracted / 'mdk-sdk'
        validate(candidate, package)
        prepare_headers(candidate, target)
        if sdk.exists():
            sdk.rename(sdk.with_name('mdk-sdk-previous-' + str(time.time_ns())))
        candidate.rename(sdk)
    return sdk.resolve()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('target', choices=('windows', 'macos', 'linux', 'ios', 'android'))
    args = parser.parse_args()
    try:
        print(str(install(args.target)) + '/')
    except (ValueError, OSError, tarfile.TarError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'MDK dependency: {error}\n')
