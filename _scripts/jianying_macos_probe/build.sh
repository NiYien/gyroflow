#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
if [[ "$(uname -s)" != Darwin ]]; then echo "Build this probe on macOS." >&2; exit 1; fi
if [[ $# != 2 ]]; then echo "Usage: bash build.sh /path/to/openfx/include /path/to/output" >&2; exit 1; fi
include_dir="$1"
output_dir="$2"
source_dir="$(cd "$(dirname "$0")" && pwd)"
bundle="$output_dir/GyroflowJianyingMacProbe.ofx.bundle"
mkdir -p "$bundle/Contents/MacOS"
xcrun clang++ -std=c++17 -dynamiclib -arch arm64 -arch x86_64 -mmacosx-version-min=11.0 \
    -I "$include_dir" "$source_dir/probe.cpp" -o "$bundle/Contents/MacOS/GyroflowJianyingMacProbe.ofx"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>xyz.niyien.gyroflow.jianying.macos.capability-probe</string>
<key>CFBundleExecutable</key><string>GyroflowJianyingMacProbe.ofx</string>
<key>CFBundleName</key><string>Gyroflow Jianying Mac capability probe</string>
<key>CFBundlePackageType</key><string>BNDL</string>
<key>CFBundleVersion</key><string>0.1</string>
</dict></plist>
PLIST
if [[ -n "${SIGNING_IDENTITY:-}" ]]; then
    codesign --force --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$bundle/Contents/MacOS/GyroflowJianyingMacProbe.ofx"
    codesign --force --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$bundle"
else
    codesign --force --sign - "$bundle/Contents/MacOS/GyroflowJianyingMacProbe.ofx"
    codesign --force --sign - "$bundle"
fi
codesign --verify --deep --strict "$bundle"
lipo -archs "$bundle/Contents/MacOS/GyroflowJianyingMacProbe.ofx"
echo "$bundle"
