import argparse
import os
import stat
import tarfile
from pathlib import Path


APPIMAGE_NAME = "gyroflow-niyien-linux64.AppImage"
TAR_NAME = "gyroflow-niyien-linux64.tar.gz"
TAR_BINARY = "Gyroflow/gyroflow-niyien"
REQUIRED_TAR_PREFIXES = (
    "Gyroflow/lib/",
    "Gyroflow/camera_db/",
)
REQUIRED_TAR_FILES = (
    TAR_BINARY,
    "Gyroflow/camera_presets/profiles.cbor.gz",
)


def verify_packages(
    appimage: Path,
    archive: Path,
    *,
    require_host_executable: bool = True,
) -> dict[str, int | list[str]]:
    appimage = Path(appimage)
    archive = Path(archive)
    if appimage.name != APPIMAGE_NAME:
        raise ValueError(f"Unexpected Linux AppImage filename: {appimage.name}")
    if archive.name != TAR_NAME:
        raise ValueError(f"Unexpected Linux tar filename: {archive.name}")
    for package in (appimage, archive):
        if not package.is_file() or package.stat().st_size == 0:
            raise ValueError(f"Linux package is missing or empty: {package}")

    if appimage.read_bytes()[:4] != b"\x7fELF":
        raise ValueError(f"Linux AppImage does not have an ELF header: {appimage}")
    if require_host_executable and os.name == "posix":
        if appimage.stat().st_mode & stat.S_IXUSR == 0:
            raise ValueError(f"Linux AppImage is not owner-executable: {appimage}")

    with tarfile.open(archive, "r:gz") as package:
        members = package.getmembers()
        names = [member.name.removeprefix("./") for member in members]
        by_name = {member.name.removeprefix("./"): member for member in members}
        for required in REQUIRED_TAR_FILES:
            member = by_name.get(required)
            if member is None or not member.isfile() or member.size == 0:
                raise ValueError(f"Linux tar is missing required non-empty file: {required}")
        for prefix in REQUIRED_TAR_PREFIXES:
            if not any(name.startswith(prefix) and by_name[name].isfile() and by_name[name].size > 0 for name in names):
                raise ValueError(f"Linux tar is missing required payload under: {prefix}")
        if by_name[TAR_BINARY].mode & stat.S_IXUSR == 0:
            raise ValueError(f"Linux tar application payload is not owner-executable: {TAR_BINARY}")

    return {
        "appimage_size": appimage.stat().st_size,
        "archive_size": archive.stat().st_size,
        "tar_members": names,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate Linux x86_64 application packages.")
    parser.add_argument("appimage", type=Path)
    parser.add_argument("archive", type=Path)
    args = parser.parse_args()
    metadata = verify_packages(args.appimage, args.archive)
    print(
        f"Validated Linux packages: appimage_size={metadata['appimage_size']} "
        f"archive_size={metadata['archive_size']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
