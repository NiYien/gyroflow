# SPDX-License-Identifier: GPL-3.0-or-later
"""Build the macOS R3D plugin against pinned public MDK and RED SDK inputs."""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / 'ext' / 'r3d-preview'
PACKAGES = (
    ('R3DSDK9.2.1.7z', 'https://downloads.sourceforge.net/project/mdk-sdk/deps/r3d/R3DSDK9.2.1.7z',
     '51cad597d71610cc69f379a39b73e777db39c7db881b7eafa13c47f0f604a919', 'R3DSDK/Include/R3DSDK.h'),
    ('mdk-abi-sdk-0.39.zip', 'https://github.com/wang-bin/mdk-sdk/releases/download/v0.39.0/mdk-abi-sdk.zip',
     '7cfad8f4d64db926176becfab4918df65e90647aae5348e67e6fbd73dd13364d', 'include/abi/mdk/FrameReader.h'),
)


def install_inputs():
    CACHE.mkdir(parents=True, exist_ok=True)
    for name, url, expected, sentinel in PACKAGES:
        archive = CACHE / name
        stamp = CACHE / (name + '.verified')
        if (CACHE / sentinel).is_file() and stamp.is_file() and stamp.read_text() == expected:
            continue
        if not archive.is_file() or hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
            with urllib.request.urlopen(url, timeout=60) as response, tempfile.NamedTemporaryFile(dir=CACHE, delete=False) as output:
                temporary = Path(output.name)
                try:
                    shutil.copyfileobj(response, output)
                except BaseException:
                    output.close()
                    temporary.unlink(missing_ok=True)
                    raise
            try:
                if hashlib.sha256(temporary.read_bytes()).hexdigest() != expected:
                    raise RuntimeError('SHA256 mismatch: ' + name)
                temporary.replace(archive)
            finally:
                temporary.unlink(missing_ok=True)
        if name.endswith('.zip'):
            with zipfile.ZipFile(archive) as package:
                for member in package.namelist():
                    if not (CACHE / member).resolve().is_relative_to(CACHE.resolve()):
                        raise RuntimeError('Unsafe archive member: ' + member)
                package.extractall(CACHE)
        else:
            # macOS libarchive supports this hash-verified 7z development package.
            subprocess.run(['/usr/bin/tar', '-xf', str(archive), '-C', str(CACHE)], check=True)
        if not (CACHE / sentinel).is_file():
            raise RuntimeError('Incomplete SDK package: ' + name)
        stamp.write_text(expected)


def build(sdk, output):
    install_inputs()
    source = ROOT / 'vendor' / 'mdk-r3d'
    sdk = sdk.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    compiler = subprocess.check_output(['xcrun', '--sdk', 'macosx', '--find', 'clang++'], text=True).strip()
    sysroot = subprocess.check_output(['xcrun', '--sdk', 'macosx', '--show-sdk-path'], text=True).strip()
    with tempfile.TemporaryDirectory(dir=output.parent) as work:
        library = Path(work) / 'libmdk-r3d.dylib'
        command = [compiler, '-std=c++2b', '-O2', '-fobjc-arc', '-fno-rtti', '-dynamiclib',
                   '-arch', 'arm64', '-arch', 'x86_64', '-mmacosx-version-min=11.0', '-isysroot', sysroot,
                   '-DBUILD_MDK_LIB', '-DMDK_ABI=abi', '-I', str(CACHE / 'include/abi'),
                   '-I', str(CACHE / 'R3DSDK/Include'), '-F', str(sdk / 'lib'), '-framework', 'mdk',
                   '-framework', 'Metal', '-framework', 'CoreGraphics', '-framework', 'CoreFoundation',
                   '-framework', 'Foundation', '-L', str(CACHE / 'R3DSDK/Lib/mac64'), '-lR3DSDK-libcpp',
                   '-L', str(source / 'sdk/lib'), '-weak-lREDDecoder', '-weak-lREDR3D',
                   '-weak-lREDMetal', '-weak-lREDOpenCL', '-Wl,-install_name,@rpath/libmdk-r3d.dylib']
        command += [str(source / name) for name in ('R3DReader.cpp', 'R3DCxxAbi.cpp', 'Debayer.cpp', 'MetalDebayer.mm')]
        subprocess.run(command + ['-o', str(library)], check=True)
        subprocess.run(['codesign', '--force', '--sign', '-', str(library)], check=True)
        os.replace(library, output)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--mdk-sdk', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    build(args.mdk_sdk, args.output)
