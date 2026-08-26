# iOS Video Source Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 仅在 iOS 上让主页面和渲染队列“添加文件”同时支持照片图库视频与文件/外接存储，并保留渲染队列现有文件夹入口。

**Architecture:** 在 iOS 构建中加入一个独立 Objective-C++ PhotosUI 桥接，使用 `PHPickerViewController` 多选视频，把临时 provider URL 复制到应用缓存后，通过现有 `Filesystem.urls_opened` 信号返回 QML。`App.qml` 统一管理来源对话框和 pending callback，主页面与渲染队列只提供各自的加载回调；现有 `FileDialog`/`FolderDialog` 继续负责内部、iCloud 和外接存储。

**Tech Stack:** Rust 2024、qmetaobject-rs、Qt 6.7.3/QML、Objective-C++、PhotosUI、UniformTypeIdentifiers、Cargo/cpp_build

**Spec:** `docs/superpowers/specs/2026-08-26-ios-video-source-picker-design.md`

## Global Constraints

- 行为变更只允许发生在 `Qt.platform.os === "ios"` 或 `#[cfg(target_os = "ios")]` 分支。
- Android、macOS、Windows 和 Linux 的 picker 入口与回调路径必须保持不变。
- Photos picker 只展示视频，`selectionLimit = 0`，并使用 `PHPickerConfigurationAssetRepresentationModeCurrent`。
- provider 返回的临时 URL 必须在 completion handler 内复制到 `QStandardPaths::CacheLocation/ios-photo-imports/<picker-uuid>/<item-uuid>/`。
- URL 批量回调必须保持用户选择顺序；部分失败时加载成功项并汇总提示失败项。
- 渲染队列“添加文件夹”继续使用现有 `FolderDialog`，不显示照片来源。
- 不启用旧的 `QIosOptionalPlugin_NSPhotoLibrary`。
- 不暂存或提交用户现有的 `.gitignore` 与 AppIcon 改动。

## File Structure

- Create `_deployment/ios/ios_video_picker.h`: C++ 可调用的 iOS picker/缓存清理边界。
- Create `_deployment/ios/ios_video_picker.mm`: PhotosUI 呈现、异步导入、顺序聚合、错误与取消回调。
- Modify `build.rs`: 仅 iOS 编译 `.mm`、增加 include path、链接 `PhotosUI`。
- Modify `src/util.rs`: 从已有 `globalUrlCatcherPtr` 调用原生桥接。
- Modify `src/controller.rs`: 给 QML 暴露 `open_ios_video_picker`、错误回调，并存放源码契约测试。
- Modify `src/gyroflow.rs`: URL catcher 建立后清理上一会话照片导入缓存。
- Modify `src/ui/App.qml`: 统一 iOS 视频来源对话框、pending callback 与错误/取消清理。
- Modify `src/ui/RenderQueue.qml`: “添加文件”接入统一来源函数；“添加文件夹”不变。
- Modify `resources/translations/gyroflow.ts`: 登记新的 App 上下文源字符串。
- Modify `resources/translations/zh_CN.ts` and regenerate `resources/translations/zh_CN.qm`: 提供简体中文文案。

---

### Task 1: Implement the native PhotosUI bridge and iOS build integration

**Files:**
- Create: `_deployment/ios/ios_video_picker.h`
- Create: `_deployment/ios/ios_video_picker.mm`
- Modify: `build.rs:269-306`
- Modify: `src/controller.rs` after the `Filesystem` implementation
- Test: `src/controller.rs`

**Interfaces:**
- Consumes: repository files through `env!("CARGO_MANIFEST_DIR")`.
- Produces: `ios_video_picker_native_contract`, `bool gyroflowIosOpenVideoPicker(QObject *receiver)`, and `void gyroflowIosCleanupVideoImports()`.

- [ ] **Step 1: Add a repository-source helper and the failing native bridge contract test**

```rust
#[cfg(test)]
mod ios_video_picker_contract_tests {
    fn source(path: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path),
        )
        .unwrap_or_default()
    }

    #[test]
    fn ios_video_picker_native_contract() {
        let native = source("_deployment/ios/ios_video_picker.mm");
        for required in [
            "PHPickerFilter videosFilter",
            "selectionLimit = 0",
            "PHPickerConfigurationAssetRepresentationModeCurrent",
            "loadFileRepresentationForTypeIdentifier",
            "QStandardPaths::CacheLocation",
            "catch_urls_open",
            "catch_picker_cancelled",
            "catch_picker_error",
        ] {
            assert!(native.contains(required), "missing native picker contract: {required}");
        }
    }
}
```

- [ ] **Step 2: Run the focused test and verify it fails for missing implementation**

Run:

```bash
cargo test ios_video_picker_native_contract -- --nocapture
```

Expected: FAIL because `_deployment/ios/ios_video_picker.mm` does not exist.

- [ ] **Step 3: Declare the small C++ bridge boundary**

```cpp
#pragma once

class QObject;

bool gyroflowIosOpenVideoPicker(QObject *receiver);
void gyroflowIosCleanupVideoImports();
```

- [ ] **Step 4: Implement picker configuration and top-view-controller lookup**

In `_deployment/ios/ios_video_picker.mm`, import UIKit, PhotosUI, UniformTypeIdentifiers, Objective-C runtime, and Qt Core types. Implement `gyroflowIosOpenVideoPicker` so it rejects a null receiver or a second active picker, creates this configuration, and presents it on the main thread:

```objc
PHPickerConfiguration *configuration =
    [[[PHPickerConfiguration alloc] initWithPhotoLibrary:[PHPhotoLibrary sharedPhotoLibrary]] autorelease];
configuration.filter = [PHPickerFilter videosFilter];
configuration.selectionLimit = 0;
configuration.preferredAssetRepresentationMode =
    PHPickerConfigurationAssetRepresentationModeCurrent;
```

Walk `presentedViewController`, `visibleViewController`, and `selectedViewController` so presentation works from the Qt root controller in phone and tablet layouts.

- [ ] **Step 5: Implement ordered asynchronous imports**

Use a per-picker session object containing `QPointer<QObject>`, an indexed result array, an indexed error array, and a `dispatch_group_t`. For each `PHPickerResult`:

```objc
[provider loadFileRepresentationForTypeIdentifier:typeIdentifier
                                 completionHandler:^(NSURL *url, NSError *error) {
    // Before returning from this block, copy url into:
    // CacheLocation/ios-photo-imports/<picker uuid>/<item uuid>/<filename>.<ext>
    // Store the copied path at the original picker index, then leave the group.
}];
```

Choose the first registered identifier whose `UTType` conforms to `UTTypeMovie`; fall back to `UTTypeMovie.identifier`. Preserve `provider.suggestedName`, derive a missing extension from `UTType.preferredFilenameExtension`, and isolate every item in a UUID directory to avoid name collisions.

- [ ] **Step 6: Deliver success, partial failure, and cancellation through Qt**

On an empty result list, invoke `catch_picker_cancelled`. After the dispatch group finishes, build a `QStringList` in original selection order and invoke:

```cpp
QMetaObject::invokeMethod(receiver, "catch_urls_open", Qt::QueuedConnection,
                          Q_ARG(QStringList, urls));
QMetaObject::invokeMethod(receiver, "catch_picker_error", Qt::QueuedConnection,
                          Q_ARG(QString, errorSummary));
```

Only call `catch_urls_open` when at least one copy succeeded and only call `catch_picker_error` when at least one item failed. Clear the active-picker guard on every terminal path and protect the receiver with `QPointer<QObject>`.

- [ ] **Step 7: Implement cache cleanup**

`gyroflowIosCleanupVideoImports()` removes and recreates only the exact `QStandardPaths::CacheLocation/ios-photo-imports` directory. It must not touch the wider Qt cache directory.

- [ ] **Step 8: Compile and link the bridge only for iOS**

Inside `build.rs`'s `target_os == "ios"` branch:

```rust
println!("cargo:rerun-if-changed=_deployment/ios/ios_video_picker.h");
println!("cargo:rerun-if-changed=_deployment/ios/ios_video_picker.mm");
config.include("_deployment/ios");
config.file("_deployment/ios/ios_video_picker.mm");
```

Add `"PhotosUI"` beside the existing `"Photos"` framework. Do not import or enable `QIosOptionalPlugin_NSPhotoLibrary`.

- [ ] **Step 9: Run the native contract test**

Run:

```bash
cargo test ios_video_picker_native_contract -- --nocapture
```

Expected: PASS.

- [ ] **Step 10: Commit the native bridge and its passing test**

```bash
git add _deployment/ios/ios_video_picker.h _deployment/ios/ios_video_picker.mm build.rs src/controller.rs
git commit -m "feat(ios): add native multi-video photo picker"
```

---

### Task 2: Connect PhotosUI to the Rust/Qt Filesystem object

**Files:**
- Modify: `src/util.rs:120-270`
- Modify: `src/controller.rs:6140-6290`
- Modify: `src/gyroflow.rs:260-265`
- Test: `src/controller.rs`

**Interfaces:**
- Consumes: `gyroflowIosOpenVideoPicker(QObject *)` and `gyroflowIosCleanupVideoImports()` from Task 1.
- Produces: QML method `filesystem.open_ios_video_picker() -> bool`, Qt method `catch_picker_error(QString)`, signal `picker_error(QString)`, and startup cleanup.

- [ ] **Step 1: Extend the contract test with the Qt bridge names**

Add assertions to `ios_video_picker_native_contract`:

```rust
let controller = source("src/controller.rs");
let util = source("src/util.rs");
assert!(controller.contains("open_ios_video_picker: qt_method!"));
assert!(controller.contains("picker_error: qt_signal!"));
assert!(util.contains("gyroflowIosOpenVideoPicker(globalUrlCatcherPtr)"));
assert!(util.contains("gyroflowIosCleanupVideoImports()"));
```

- [ ] **Step 2: Run the focused test and verify the new assertions fail**

Run:

```bash
cargo test ios_video_picker_native_contract -- --nocapture
```

Expected: FAIL on the missing Rust/Qt bridge strings.

- [ ] **Step 3: Add util wrappers guarded by `Q_OS_IOS` / Rust cfg**

In the global `cpp!` include block:

```cpp
#ifdef Q_OS_IOS
#  include "ios_video_picker.h"
#endif
```

Add Rust functions with non-iOS-safe behavior:

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

- [ ] **Step 4: Expose methods and signals on `Filesystem`**

Add:

```rust
open_ios_video_picker: qt_method!(fn(&self) -> bool),
catch_picker_error: qt_method!(fn(&self, message: QString)),
picker_error: qt_signal!(message: QString),
```

and implementations:

```rust
fn open_ios_video_picker(&self) -> bool {
    util::open_ios_video_picker()
}

fn catch_picker_error(&self, message: QString) {
    self.picker_error(message);
}
```

Keep the existing `catch_urls_open`, `catch_picker_cancelled`, `urls_opened`, and `picker_cancelled` signatures unchanged.

- [ ] **Step 5: Clean previous photo-import cache after installing the URL catcher**

Immediately after `util::set_url_catcher(...)` in `src/gyroflow.rs`, call:

```rust
util::cleanup_ios_video_imports();
```

The function is a no-op on every non-iOS build.

- [ ] **Step 6: Run focused and host regression tests**

Run:

```bash
cargo test ios_video_picker_native_contract -- --nocapture
cargo test
```

Expected: the native/bridge contract passes and all existing library tests pass.

- [ ] **Step 7: Commit the Qt bridge**

```bash
git add src/util.rs src/controller.rs src/gyroflow.rs
git commit -m "feat(ios): bridge photo picker results into QML"
```

---

### Task 3: Add the shared iOS source chooser to all three relevant entrances

**Files:**
- Modify: `src/ui/App.qml:64-95,697-750`
- Modify: `src/ui/RenderQueue.qml:868-982`
- Test: `src/controller.rs::ios_video_picker_contract_tests::ios_video_source_routing_contract`

**Interfaces:**
- Consumes: `filesystem.open_ios_video_picker()`, `filesystem.urls_opened`, `filesystem.picker_cancelled`, and `filesystem.picker_error` from Task 2.
- Produces: `window.openVideoSourcePicker(callback, fallbackDialog)` used by the main viewer and queue “Add files”; queue “Add folder” remains on `openPicker(1, ...)`.

- [ ] **Step 1: Add the failing QML routing contract test**

Append this test to `ios_video_picker_contract_tests`:

```rust
#[test]
fn ios_video_source_routing_contract() {
    let app = source("src/ui/App.qml");
    let queue = source("src/ui/RenderQueue.qml");
    assert!(app.contains("function openVideoSourcePicker("));
    assert!(app.contains("Qt.platform.os === \"ios\""));
    assert!(app.contains("filesystem.open_ios_video_picker()"));
    assert!(app.contains("videoArea.loadMultipleFiles(urls, false)"));
    assert!(queue.contains("window.openVideoSourcePicker(function(urls)"));
    assert!(queue.contains("dt.loadFiles(urls)"));
    assert!(queue.contains("window.openPicker(1, false"));
}
```

- [ ] **Step 2: Run the focused test and verify it fails**

Run:

```bash
cargo test ios_video_source_routing_contract -- --nocapture
```

Expected: FAIL because `openVideoSourcePicker` does not exist.

- [ ] **Step 3: Add a centralized source chooser in `App.qml`**

Place this beside `openPicker`:

```qml
function openVideoSourcePicker(callback: var, fallbackDialog: var): void {
    if (Qt.platform.os !== "ios") {
        window.openPicker(0, true, callback, fallbackDialog);
        return;
    }
    window.messageBox(Modal.Question, qsTr("Choose video source"), [
        {
            text: qsTr("Photos"),
            accent: true,
            clicked: function() {
                window.pendingPickerCallback = callback;
                if (!filesystem.open_ios_video_picker()) {
                    window.pendingPickerCallback = null;
                    window.messageBox(Modal.Error, qsTr("Unable to open the photo library."), [ { text: qsTr("Ok") } ]);
                }
            }
        },
        {
            text: qsTr("Files and external storage"),
            clicked: function() {
                if (fallbackDialog.open2) fallbackDialog.open2(); else fallbackDialog.open();
            }
        },
        { text: qsTr("Cancel") }
    ]);
}
```

- [ ] **Step 4: Route the main page through the shared chooser**

Change `openMainFileDialog` to:

```qml
function openMainFileDialog(): void {
    window.openVideoSourcePicker(function(urls) {
        videoArea.loadMultipleFiles(urls, false);
    }, fileDialog);
}
```

The three existing main-page triggers already call `openMainFileDialog`, so they require no individual changes.

- [ ] **Step 5: Handle native cancel and import errors**

In the existing `Connections { target: filesystem }` block add:

```qml
function onPicker_cancelled(): void {
    window.pendingPickerCallback = null;
}
function onPicker_error(message: string): void {
    window.pendingPickerCallback = null;
    window.messageBox(Modal.Error, qsTr("Some videos could not be imported: %1").arg(message), [ { text: qsTr("Ok") } ]);
}
```

The native bridge queues successful `urls_opened` before a partial-failure error, so successful items reach the saved callback before the error handler clears it.

- [ ] **Step 6: Route render queue “Add files” through the same chooser**

Replace only the first mobile add button handler with:

```qml
onClicked: {
    window.openVideoSourcePicker(function(urls) {
        dt.loadFiles(urls);
    }, mobileAddFilesDialog);
}
```

Leave the adjacent “Add folder” handler exactly on `window.openPicker(1, false, ...)`, including access-grant registration and `dt.loadFiles([folderUrl])`.

- [ ] **Step 7: Run the QML routing contract and existing picker tests**

Run:

```bash
cargo test ios_video_source_routing_contract -- --nocapture
cargo test mobile_file_picker -- --nocapture
cargo test main_file_dialog -- --nocapture
```

Expected: PASS. The existing tests continue to see `FileDialog.OpenFiles`, video extension filters, and Android `openPicker` behavior.

- [ ] **Step 8: Commit QML routing and its passing test**

```bash
git add src/ui/App.qml src/ui/RenderQueue.qml src/controller.rs
git commit -m "feat(ios): offer photos or external files for video input"
```

---

### Task 4: Add Simplified Chinese strings and perform full verification

**Files:**
- Modify: `resources/translations/gyroflow.ts`
- Modify: `resources/translations/zh_CN.ts`
- Modify (generated): `resources/translations/zh_CN.qm`
- Verify: all implementation files from Tasks 1-3

**Interfaces:**
- Consumes: QML strings `Choose video source`, `Photos`, `Files and external storage`, `Unable to open the photo library.`, and `Some videos could not be imported: %1`.
- Produces: App-context source catalog entries and Simplified Chinese runtime translations.

- [ ] **Step 1: Add source-catalog and zh_CN App-context messages**

Add the five source entries to the `App` context in both `.ts` files. In `zh_CN.ts`, use:

```text
Choose video source -> 选择视频来源
Photos -> 照片
Files and external storage -> 文件与外接存储
Unable to open the photo library. -> 无法打开照片图库。
Some videos could not be imported: %1 -> 部分视频无法导入：%1
```

Keep `%1` unchanged.

- [ ] **Step 2: Regenerate the Simplified Chinese binary catalog**

Run:

```bash
ext/6.7.3/macos/bin/lrelease resources/translations/zh_CN.ts -qm resources/translations/zh_CN.qm
```

Expected: exit 0 and a regenerated Qt Translation file.

- [ ] **Step 3: Run formatting and source-contract checks**

Run:

```bash
git diff --check
cargo fmt --all -- --check
cargo test ios_video_picker_contract_tests -- --nocapture
```

Expected: no whitespace errors, formatting passes, both contracts pass.

- [ ] **Step 4: Run the host regression suite**

Run:

```bash
cargo test
```

Expected: all existing and new library tests pass.

- [ ] **Step 5: Build the iOS target**

Use the same environment as `_scripts/ios.just` and build without packaging/signing:

```bash
FFMPEG_DIR="$PWD/ext/ffmpeg-8.1-iOS-gpl-lite" \
PATH="$PWD/ext/6.7.3/ios/bin:/usr/libexec:$PATH" \
QMAKE="$PWD/ext/6.7.3/ios/bin/qmake" \
OPENCV_LINK_LIBS="opencv_core4,opencv_calib3d4,opencv_features2d4,opencv_imgproc4,opencv_video4,opencv_flann4,opencv_stitching4" \
OPENCV_LINK_PATHS="$PWD/ext/vcpkg/installed/arm64-ios/lib" \
OPENCV_INCLUDE_PATHS="$PWD/ext/vcpkg/installed/arm64-ios/include/opencv4" \
cargo build --target aarch64-apple-ios
```

Expected: Objective-C++ compiles, PhotosUI resolves, and the final `gyroflow` link succeeds.

- [ ] **Step 6: Inspect the iOS binary for the new bridge**

Run:

```bash
nm -gU target/aarch64-apple-ios/debug/gyroflow | rg "gyroflowIosOpenVideoPicker"
otool -L target/aarch64-apple-ios/debug/gyroflow | rg "PhotosUI.framework"
```

Expected: bridge symbol is present and PhotosUI is linked.

- [ ] **Step 7: Commit translations and verification-ready state**

```bash
git add resources/translations/gyroflow.ts resources/translations/zh_CN.ts resources/translations/zh_CN.qm
git commit -m "i18n: translate iOS video source picker"
```

- [ ] **Step 8: Record manual-device checks for handoff**

Report these as pending unless a signed iOS device run was actually performed: main-page Photos single/multi-select, queue Photos multi-select, iCloud-only video, duplicate names, cancel, partial failure, “On My iPhone”, iCloud Drive, USB/SD file, and USB/SD folder.
