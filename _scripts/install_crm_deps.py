# SPDX-License-Identifier: GPL-3.0-or-later
"""Install pinned source/header inputs for the desktop CRM decoder."""
import hashlib
from pathlib import Path
import shutil
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / 'ext' / 'crm'
PACKAGES = [
    ('LibRaw-0.22.2-Win64.zip', 'https://www.libraw.org/data/LibRaw-0.22.2-Win64.zip',
     'ac64fa12bb00a7581332d4c6ab918c0533fb3f119d6b668d47a6875410dca948', 'LibRaw-0.22.2/libraw/libraw.h'),
    ('mdk-abi-sdk-0.36.zip', 'https://github.com/wang-bin/mdk-sdk/releases/download/v0.36.0/mdk-abi-sdk.zip',
     '259de2bd9326c19d7b277cea210f99daaa7a4fc2251711748f04e61402308e86', 'include/abi/mdk/VideoDecoder.h'),
]


def install():
    DEST.mkdir(parents=True, exist_ok=True)
    for name, url, expected, header in PACKAGES:
        archive = DEST / name
        stamp = DEST / (name + '.verified')
        if stamp.exists() and stamp.read_text() == expected and (DEST / header).exists():
            continue
        if not archive.exists() or hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
            print(f'Downloading {name}', flush=True)
            with urllib.request.urlopen(url, timeout=60) as response, tempfile.NamedTemporaryFile(dir=DEST, delete=False) as output:
                temporary = Path(output.name)
                try:
                    shutil.copyfileobj(response, output)
                except BaseException:
                    output.close()
                    temporary.unlink(missing_ok=True)
                    raise
            try:
                if hashlib.sha256(temporary.read_bytes()).hexdigest() != expected:
                    raise RuntimeError(f'SHA256 mismatch: {name}')
                temporary.replace(archive)
            finally:
                temporary.unlink(missing_ok=True)
        with zipfile.ZipFile(archive) as package:
            for item in package.infolist():
                target = (DEST / item.filename).resolve()
                if not target.is_relative_to(DEST.resolve()):
                    raise RuntimeError(f'Unsafe archive path: {item.filename}')
            package.extractall(DEST)
        stamp.write_text(expected)
    print('CRM source dependencies ready', flush=True)


if __name__ == '__main__':
    install()
