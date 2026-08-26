# iOS Video Source Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 仅在 iOS 上让主页面和渲染队列“添加文件”同时支持照片图库视频与文件/外接存储，并保留渲染队列现有文件夹入口。

**Architecture:** iOS 构建加入一个 Objective-C++ PhotosUI 桥接，使用 `PHPickerViewController` 多选视频，把 provider 临时 URL 复制到应用缓存，再通过既有 `Filesystem.urls_opened` 返回 QML。一个独立、可实例化测试的 `VideoSourcePicker.qml` 负责来源对话框和 callback 生命周期；主页面与渲染队列共享该组件，现有文件/文件夹 picker 继续负责本机、iCloud 和外接存储。

**Tech Stack:** Rust 2024、qmetaobject-rs、Qt 6.7.3/QML/Qt Quick Test、Objective-C++、PhotosUI、UniformTypeIdentifiers、Cargo/cpp_build

**Spec:** `docs/superpowers/specs/2026-08-26-ios-video-source-picker-design.md`

## Global Constraints

- 行为变化只出现在 `Qt.platform.os === "ios"` 或 iOS 编译分支。
- Photos picker 使用 `videosFilter`、`selectionLimit = 0` 和 `PHPickerConfigurationAssetRepresentationModeCurrent`。
- provider 临时 URL 在 completion handler 内复制到 `CacheLocation/ios-photo-imports/<picker UUID>/<item UUID>/`。
- 成功 URL 保持用户选择顺序；部分失败仍加载成功项并汇总失败项。
- 渲染队列“添加文件夹”继续使用现有 `FolderDialog`。
- 不启用 `QIosOptionalPlugin_NSPhotoLibrary`。
- 不暂存或提交用户现有 `.gitignore` 与 AppIcon 改动。
- 当前基线：host build 通过；host tests 为 735 pass / 10 个既有失败。最终不得增加失败项。

## File Structure

- Create `_deployment/ios/ios_video_picker.h`: Qt/Rust 可调用的原生边界。
- Create `_deployment/ios/ios_video_picker.mm`: PhotosUI 与缓存导入实现。
- Create `_scripts/test_ios_video_picker_bridge.sh`: 真正编译 Objective-C++ bridge 的 iOS 测试。
- Modify `build.rs`: iOS 编译和 PhotosUI 链接。
- Modify `src/util.rs`, `src/controller.rs`, `src/gyroflow.rs`: Qt QObject 桥接与缓存清理。
- Create `src/ui/components/VideoSourcePicker.qml`: 可测试的统一来源协调器。
- Create `tests/qml/tst_video_source_picker.qml`: 来源、callback、取消和错误行为测试。
- Modify `src/ui/components/qmldir`, `src/resources_qml.rs`: 注册组件。
- Modify `src/ui/App.qml`, `src/ui/RenderQueue.qml`: 主页面和队列接入。
- Modify `resources/translations/gyroflow.ts`, `zh_CN.ts`, `zh_CN.qm`: 文案。

---

### Task 1: Native PhotosUI bridge

**Files:**
- Create: `_scripts/test_ios_video_picker_bridge.sh`
- Create: `_deployment/ios/ios_video_picker.h`
- Create: `_deployment/ios/ios_video_picker.mm`
- Modify: `build.rs:269-306`

**Interfaces:**
- Consumes: `QObject *receiver` with `catch_urls_open(QStringList)`, `catch_picker_cancelled()`, `catch_picker_error(QString)`.
- Produces: `bool gyroflowIosOpenVideoPicker(QObject *)`, `void gyroflowIosCleanupVideoImports()`.

- [ ] **Step 1: Write the failing bridge compile test**

```sh
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
```

- [ ] **Step 2: Run it and observe RED**

Run: `sh _scripts/test_ios_video_picker_bridge.sh`

Expected: non-zero because the header/source do not exist.

- [ ] **Step 3: Declare the bridge API**

```cpp
#pragma once
class QObject;
bool gyroflowIosOpenVideoPicker(QObject *receiver);
void gyroflowIosCleanupVideoImports();
```

- [ ] **Step 4: Implement picker presentation**

In `.mm`, import UIKit, Photos, PhotosUI, UniformTypeIdentifiers, Objective-C runtime and Qt Core. Reject null receiver or an already-active picker. Resolve the top view controller by walking presented/navigation/tab controllers. Configure:

```objc
PHPickerConfiguration *configuration =
    [[[PHPickerConfiguration alloc] initWithPhotoLibrary:[PHPhotoLibrary sharedPhotoLibrary]] autorelease];
configuration.filter = [PHPickerFilter videosFilter];
configuration.selectionLimit = 0;
configuration.preferredAssetRepresentationMode =
    PHPickerConfigurationAssetRepresentationModeCurrent;
```

Retain the delegate with `objc_setAssociatedObject` because `PHPickerViewController.delegate` is not the lifetime owner.

- [ ] **Step 5: Implement ordered imports**

For each result, choose the first registered `UTType` conforming to `UTTypeMovie`, then call:

```objc
[provider loadFileRepresentationForTypeIdentifier:typeIdentifier
                                 completionHandler:^(NSURL *url, NSError *error) {
    // Copy synchronously inside this callback into the session/item UUID directory.
    // Store either the copied path or a localized error at the original result index.
    dispatch_group_leave(group);
}];
```

Use one `dispatch_group`, indexed `NSMutableArray` values, and `@synchronized(session)` so callback completion order cannot reorder output. Preserve `suggestedName`; derive a missing extension from `UTType.preferredFilenameExtension`.

- [ ] **Step 6: Deliver terminal outcomes**

Empty results invoke `catch_picker_cancelled`. Group completion builds `QStringList` in index order and invokes `catch_urls_open` only when non-empty; an aggregate `QString` invokes `catch_picker_error` only when failures exist. Use `QPointer<QObject>` and `Qt::QueuedConnection`. Clear the active guard on every terminal path.

- [ ] **Step 7: Implement exact cache cleanup**

`gyroflowIosCleanupVideoImports()` removes and recreates only `QStandardPaths::CacheLocation/ios-photo-imports`, never the parent cache directory.

- [ ] **Step 8: Add iOS build integration**

```rust
println!("cargo:rerun-if-changed=_deployment/ios/ios_video_picker.h");
println!("cargo:rerun-if-changed=_deployment/ios/ios_video_picker.mm");
config.include("_deployment/ios");
config.file("_deployment/ios/ios_video_picker.mm");
```

Add `"PhotosUI"` next to existing `"Photos"` in the iOS framework list.

- [ ] **Step 9: Verify GREEN**

Run: `sh _scripts/test_ios_video_picker_bridge.sh`

Expected: Objective-C++ syntax check exits 0.

- [ ] **Step 10: Commit**

```bash
git add _scripts/test_ios_video_picker_bridge.sh _deployment/ios/ios_video_picker.h _deployment/ios/ios_video_picker.mm build.rs
git commit -m "feat(ios): add native multi-video photo picker"
```

---

### Task 2: Rust/Qt bridge

**Files:**
- Modify: `src/util.rs:120-270`
- Modify: `src/controller.rs:6140-6290`
- Modify: `src/gyroflow.rs:260-265`

**Interfaces:**
- Consumes: native functions from Task 1.
- Produces: `filesystem.open_ios_video_picker() -> bool`, `catch_picker_error(QString)`, `picker_error(QString)`.

- [ ] **Step 1: Include the bridge and add util wrappers**

```cpp
#ifdef Q_OS_IOS
#  include "ios_video_picker.h"
#endif
```

```rust
pub fn open_ios_video_picker() -> bool {
    cpp!(unsafe [] -> bool as "bool" {
        #ifdef Q_OS_IOS
            return gyroflowIosOpenVideoPicker(globalUrlCatcherPtr);
        #else
            return false;
        #endif
    })
}

pub fn cleanup_ios_video_imports() {
    cpp!(unsafe [] {
        #ifdef Q_OS_IOS
            gyroflowIosCleanupVideoImports();
        #endif
    });
}
```

- [ ] **Step 2: Expose QObject methods/signals**

```rust
open_ios_video_picker: qt_method!(fn(&self) -> bool),
catch_picker_error: qt_method!(fn(&self, message: QString)),
picker_error: qt_signal!(message: QString),
```

Implement the first by calling `util::open_ios_video_picker()` and the second by emitting `self.picker_error(message)`. Keep existing URL/cancel signatures unchanged.

- [ ] **Step 3: Clean stale imports at startup**

Immediately after `util::set_url_catcher(fspinned.get_or_create_cpp_object());`, call `util::cleanup_ios_video_imports()`. It is a no-op outside iOS.

- [ ] **Step 4: Verify compilation**

Run:

```bash
sh _scripts/test_ios_video_picker_bridge.sh
cargo test --no-run
```

Expected: native syntax and host Rust/Qt test binary compilation pass.

- [ ] **Step 5: Commit**

```bash
git add src/util.rs src/controller.rs src/gyroflow.rs
git commit -m "feat(ios): bridge photo picker results into QML"
```

---

### Task 3: Tested QML source coordinator and all entrances

**Files:**
- Create: `tests/qml/tst_video_source_picker.qml`
- Create: `src/ui/components/VideoSourcePicker.qml`
- Modify: `src/ui/components/qmldir`
- Modify: `src/resources_qml.rs`
- Modify: `src/ui/App.qml:64-95,697-750`
- Modify: `src/ui/RenderQueue.qml:868-982`

**Interfaces:**
- Consumes: `filesystem.open_ios_video_picker`, `picker_cancelled`, `picker_error`, host `openPicker`/`messageBox`/`pendingPickerCallback`.
- Produces: `VideoSourcePicker.open(platformOs, callback, fallbackDialog)`.

- [ ] **Step 1: Write a failing Qt Quick Test**

The test creates real `VideoSourcePicker` instances with fake boundary QObjects. It verifies these literal outcomes:

```qml
function test_nonIosUsesExistingPicker() {
    picker.open("android", selectedCallback, fallback)
    compare(host.openPickerCalls, 1)
    compare(host.lastMode, 0)
    compare(host.lastAllowMultiple, true)
}

function test_iosPhotosUsesNativePicker() {
    picker.open("ios", selectedCallback, fallback)
    compare(host.lastButtons.length, 3)
    host.lastButtons[0].clicked()
    compare(filesystem.nativeOpenCalls, 1)
    compare(host.pendingPickerCallback, selectedCallback)
}

function test_iosFilesUsesDocumentPicker() {
    picker.open("ios", selectedCallback, fallback)
    host.lastButtons[1].clicked()
    compare(fallback.open2Calls, 1)
    compare(filesystem.nativeOpenCalls, 0)
}

function test_cancelAndErrorClearPendingCallback() {
    picker.open("ios", selectedCallback, fallback)
    host.lastButtons[0].clicked()
    filesystem.picker_cancelled()
    compare(host.pendingPickerCallback, null)
    host.pendingPickerCallback = selectedCallback
    filesystem.picker_error("clip.mov")
    compare(host.pendingPickerCallback, null)
    compare(host.messageBoxCalls, 2)
}
```

- [ ] **Step 2: Run it and observe RED**

Run: `ext/6.7.3/macos/bin/qmltestrunner -input tests/qml/tst_video_source_picker.qml -platform offscreen`

Expected: fail because `VideoSourcePicker.qml` does not exist.

- [ ] **Step 3: Implement `VideoSourcePicker.qml`**

Create a zero-size `Item` with `hostObject`, `filesystemObject`, `questionType`, and `errorType` properties. Its `open()` method:

```qml
function open(platformOs: string, callback: var, fallbackDialog: var): void {
    if (platformOs !== "ios") {
        hostObject.openPicker(0, true, callback, fallbackDialog)
        return
    }
    hostObject.messageBox(questionType, qsTr("Choose video source"), [
        { text: qsTr("Photos"), accent: true, clicked: function() {
            hostObject.pendingPickerCallback = callback
            if (!filesystemObject.open_ios_video_picker()) {
                hostObject.pendingPickerCallback = null
                hostObject.messageBox(errorType, qsTr("Unable to open the photo library."), [ { text: qsTr("Ok") } ])
            }
        }},
        { text: qsTr("Files and external storage"), clicked: function() {
            if (fallbackDialog.open2) fallbackDialog.open2(); else fallbackDialog.open()
        }},
        { text: qsTr("Cancel") }
    ])
}
```

Connections clear `hostObject.pendingPickerCallback` on cancel, and on error also call `hostObject.messageBox(errorType, qsTr("Some videos could not be imported: %1").arg(message), [ { text: qsTr("Ok") } ])`.

- [ ] **Step 4: Register and instantiate the component**

Add it to `qmldir` and `resources_qml.rs`. Instantiate once in `App.qml` with `hostObject: window`, `filesystemObject: filesystem`, and Modal enum values.

- [ ] **Step 5: Route the main page**

```qml
function openMainFileDialog(): void {
    videoSourcePicker.open(Qt.platform.os, function(urls) {
        videoArea.loadMultipleFiles(urls, false)
    }, fileDialog)
}
```

- [ ] **Step 6: Route render queue “Add files” only**

```qml
videoSourcePicker.open(Qt.platform.os, function(urls) {
    dt.loadFiles(urls)
}, mobileAddFilesDialog)
```

Leave the adjacent “Add folder” call unchanged: it continues to pass mode `1`, `allowMultiple = false`, its existing folder-access-grant callback, and `mobileAddFolderDialog` to `window.openPicker`.

- [ ] **Step 7: Verify GREEN and focused regressions**

Run:

```bash
ext/6.7.3/macos/bin/qmltestrunner -input tests/qml/tst_video_source_picker.qml -platform offscreen
cargo test mobile_file_picker -- --nocapture
cargo test main_file_dialog -- --nocapture
```

Expected: new QML tests pass; focused existing tests pass after updating only stale expectations that assert the old direct `open2()` call.

- [ ] **Step 8: Commit**

```bash
git add tests/qml/tst_video_source_picker.qml src/ui/components/VideoSourcePicker.qml src/ui/components/qmldir src/resources_qml.rs src/ui/App.qml src/ui/RenderQueue.qml src/rendering/render_queue.rs
git commit -m "feat(ios): offer photos or external files for video input"
```

---

### Task 4: Translation and complete verification

**Files:**
- Modify: `resources/translations/gyroflow.ts`
- Modify: `resources/translations/zh_CN.ts`
- Modify generated: `resources/translations/zh_CN.qm`

- [ ] **Step 1: Add App/VideoSourcePicker catalog messages**

Use these Simplified Chinese literals and keep `%1` intact:

```text
Choose video source -> 选择视频来源
Photos -> 照片
Files and external storage -> 文件与外接存储
Unable to open the photo library. -> 无法打开照片图库。
Some videos could not be imported: %1 -> 部分视频无法导入：%1
```

- [ ] **Step 2: Generate runtime catalog**

Run: `ext/6.7.3/macos/bin/lrelease resources/translations/zh_CN.ts -qm resources/translations/zh_CN.qm`

- [ ] **Step 3: Run formatting and focused tests**

```bash
git diff --check
cargo fmt --all -- --check
sh _scripts/test_ios_video_picker_bridge.sh
ext/6.7.3/macos/bin/qmltestrunner -input tests/qml/tst_video_source_picker.qml -platform offscreen
```

- [ ] **Step 4: Run host regression comparison**

Run the same environment-qualified `cargo test` used for the baseline outside the sandbox. Expected: no new failures beyond the recorded 10; all new/focused tests pass.

- [ ] **Step 5: Build iOS target**

```bash
FFMPEG_DIR="$PWD/ext/ffmpeg-8.1-iOS-gpl-lite" \
PATH="$PWD/ext/6.7.3/ios/bin:/usr/libexec:$PATH" \
QMAKE="$PWD/ext/6.7.3/ios/bin/qmake" \
OPENCV_LINK_LIBS="opencv_core4,opencv_calib3d4,opencv_features2d4,opencv_imgproc4,opencv_video4,opencv_flann4,opencv_stitching4" \
OPENCV_LINK_PATHS="$PWD/ext/vcpkg/installed/arm64-ios/lib" \
OPENCV_INCLUDE_PATHS="$PWD/ext/vcpkg/installed/arm64-ios/include/opencv4" \
cargo build --target aarch64-apple-ios
```

- [ ] **Step 6: Inspect final binary**

```bash
nm -gU target/aarch64-apple-ios/debug/gyroflow | rg gyroflowIosOpenVideoPicker
otool -L target/aarch64-apple-ios/debug/gyroflow | rg PhotosUI.framework
```

- [ ] **Step 7: Commit translations**

```bash
git add resources/translations/gyroflow.ts resources/translations/zh_CN.ts resources/translations/zh_CN.qm
git commit -m "i18n: translate iOS video source picker"
```

- [ ] **Step 8: Handoff manual device matrix**

Report as pending unless actually run on a signed device: main Photos single/multi-select, queue Photos multi-select, iCloud-only video, duplicate names, cancel, partial failure, On My iPhone, iCloud Drive, USB/SD file, USB/SD folder.
