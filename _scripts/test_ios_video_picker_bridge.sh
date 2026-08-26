#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source_file="$repo_dir/_deployment/ios/ios_video_picker.mm"
header_file="$repo_dir/_deployment/ios/ios_video_picker.h"
sdk_path=$(xcrun --sdk iphoneos --show-sdk-path)
qt_dir="$repo_dir/ext/6.7.3/ios"

test -f "$source_file"
test -f "$header_file"

xcrun --sdk iphoneos clang++ -std=c++17 -fsyntax-only -x objective-c++ \
    -target arm64-apple-ios14.0 -isysroot "$sdk_path" \
    -F "$qt_dir/lib" -I "$qt_dir/include" -I "$qt_dir/include/QtCore" \
    "$source_file"
