# iOS 视频来源选择器设计

## 背景

当前主页面和渲染队列的“添加文件”入口在 iOS 上都使用 Qt `FileDialog`。它映射到 iOS 文档选择器，因此能够访问“我的 iPhone”、iCloud Drive、文件提供商，以及系统“文件”App 可见的 USB/SD 等外接存储，但不能直接浏览“照片”中的视频。

`Info.plist` 已包含 `NSPhotoLibraryUsageDescription`，构建也已链接 Photos framework；然而权限声明和 framework 本身不会把照片图库合并进文档选择器。Qt 6.7.3 的可选照片插件使用旧的 `UIImagePickerController`，默认只显示静态图片、只适合单选，也不满足本项目的多视频导入要求。

## 目标

- 仅修改 iOS 行为；Android、macOS、Windows 和 Linux 保持不变。
- 主页面“打开视频”允许选择“照片”或“文件与外接存储”。
- 渲染队列“添加文件”提供相同的来源选择，并继续把结果送入队列批量加载流程。
- 渲染队列“添加文件夹”继续使用文件夹选择器，可访问本机、iCloud 和外接存储；它不提供照片来源，因为照片图库不是文件系统目录。
- 相册选择支持多个视频，并尽量保留原始容器、编码和元数据，避免由系统转码破坏陀螺仪数据。

## 非目标

- 不改变其他平台的选择器或权限逻辑。
- 不把照片、Live Photo 或相册专辑作为可加载内容。
- 不替换现有 iOS 文档/文件夹选择器。
- 不修改渲染队列的扫描、去重、代理配对或输出目录逻辑。

## 方案选择

### 采用：独立的 PhotosUI `PHPickerViewController`

新增一个仅在 iOS 构建的 Objective-C++ 桥接，使用 `PHPickerViewController`：

- `filter = PHPickerFilter.videosFilter`，只显示视频；
- `selectionLimit = 0`，允许不限数量的多选；
- `preferredAssetRepresentationMode = PHPickerConfigurationAssetRepresentationModeCurrent`，优先取得当前原始表示，避免兼容性转码；
- 通过 `NSItemProvider` 异步读取每个选择项，并立即复制到应用自己的缓存目录；
- 所有项目处理完毕后，复用现有的 `Filesystem.urls_opened` 信号一次性把 URL 列表交回 QML。

### 不采用：修改 Qt 可选照片插件

Qt 6.7.3 插件基于 `UIImagePickerController`，默认只显示图片，而且不支持项目要求的多选体验。直接修改该插件还会增加对自定义 Qt 静态库补丁的依赖。

### 不采用：只保留文档选择器

这要求用户先在“照片”中执行“存储到文件”，不能满足直接选择相册视频的要求。

## 用户交互

新增一个 QML 共用函数，接收“选中 URL 后的回调”和现有文件对话框：

1. 非 iOS 平台直接进入现有 `openPicker` 流程，不增加弹窗。
2. iOS 平台显示来源对话框：
   - **照片**：启动原生 PhotosUI 视频选择器；
   - **文件与外接存储**：打开调用方传入的现有 `FileDialog`；
   - **取消**：不改变当前视频或队列。
3. 主页面的回调继续调用 `videoArea.loadMultipleFiles(urls, false)`。
4. 渲染队列“添加文件”的回调继续调用 `dt.loadFiles(urls)`。
5. 渲染队列“添加文件夹”保持原样，直接打开 `FolderDialog`。

来源对话框在 `App.qml` 中统一实现，使主页面和渲染队列不会产生不同的 iOS 行为或文案。

## 原生桥接与数据流

### 接口边界

新增仅 iOS 可用的原生入口，例如：

```text
QML
  -> Filesystem.open_ios_video_picker()
  -> Rust util 包装
  -> Objective-C++ PhotosUI 桥接
  -> Filesystem.catch_urls_open(QStringList)
  -> Filesystem.urls_opened
  -> QML pendingPickerCallback
```

桥接接收现有全局 URL catcher `QObject`，完成后使用 `QMetaObject::invokeMethod(..., Qt::QueuedConnection)` 回到 Qt 主线程。这样沿用 Android 多 URL 选择已经建立的 QML 路由，不再创建第二套批量加载协议。

### 文件落地

`NSItemProvider` 回调给出的文件 URL 只在回调期间保证有效，不能直接交给播放器或渲染队列。桥接必须在回调内复制文件：

- 目标根目录使用 `QStandardPaths::CacheLocation` 下的照片导入目录；
- 每次 picker 会话创建 UUID 子目录；
- 每个选择项使用独立 UUID 子目录，避免同名视频冲突；
- 优先保留 provider 提供的文件名和扩展名；缺少扩展名时从匹配的 `UTType` 推导；
- 返回 `file://` URL；
- 保持用户的选择顺序，即使异步复制完成顺序不同。

当前会话的文件在应用运行期间不删除，确保预览和渲染队列可以持续读取。下一次启动时清理上一会话遗留的照片导入缓存，避免大视频长期占用空间；当前会话目录不能被启动清理逻辑误删。

### 部分失败

所有选择项独立处理：

- 如果至少一个视频复制成功，成功项仍按原选择顺序进入现有加载流程；
- 失败项汇总为一条用户可读错误，包含文件名或序号；
- 如果全部失败，只显示错误，不发送空 URL 列表；
- 用户在系统 picker 中取消时触发现有 `picker_cancelled` 路径并清除 `pendingPickerCallback`。

## 文件与构建变更

预计涉及：

- `src/ui/App.qml`：iOS 来源对话框、统一视频来源函数、原生 picker 信号处理；
- `src/ui/RenderQueue.qml`：“添加文件”改用统一视频来源函数，“添加文件夹”保持原样；
- `src/controller.rs`：向 QML 暴露 iOS 视频 picker 方法及错误回调；
- `src/util.rs`：连接 Rust/Qt 与 Objective-C++ 桥接，管理 URL catcher 调用；
- `_deployment/ios/ios_video_picker.h/.mm`：PhotosUI picker、异步复制、顺序聚合与错误汇总；
- `build.rs`：仅 iOS 编译 Objective-C++ 文件，并链接 `PhotosUI` framework；
- 翻译源：新增“照片”“文件与外接存储”等用户可见字符串。

不启用 `_deployment/ios/qml_plugins.cpp` 中旧的 `QIosOptionalPlugin_NSPhotoLibrary`。

## 错误处理

- 找不到可用于呈现 picker 的顶层 `UIViewController`：返回 `false`，QML 显示无法打开相册的错误，并清除 pending callback。
- provider 不包含可读取的视频表示：记录该选择项失败，继续处理其他项。
- iCloud 下载、复制或磁盘空间错误：汇总系统错误信息；成功项仍可加载。
- 原生回调到达时 QML catcher 已销毁：不调用失效对象，不崩溃。
- picker 已打开时拒绝第二次打开，避免 `pendingPickerCallback` 被后一次操作覆盖。

## 验证策略

### 自动验证

- Rust/QML 源码契约测试：iOS 才显示来源对话框；主页面和队列“添加文件”都使用统一函数；“添加文件夹”仍走文件夹 picker。
- 原生桥接源码契约测试：存在视频 filter、多选、原始表示模式、缓存复制和按选择顺序聚合。
- 主机端现有单元测试和 QML 静态测试全部通过。
- `aarch64-apple-ios` 目标完整编译和链接通过，确认 `PhotosUI` 与 Objective-C++ bridge 被包含。

### iOS 真机验证

- 主页面从照片选择一个和多个本地视频；
- 渲染队列从照片批量添加视频；
- 从 iCloud Photos 选择尚未下载到本机的视频；
- 选择同名视频，确认不会互相覆盖；
- 取消来源对话框和系统照片 picker；
- 从“我的 iPhone”、iCloud Drive 和 USB/SD 外接存储选择文件；
- 从本机或外接存储选择文件夹加入渲染队列；
- 导入较大 4K 视频后完成预览和队列渲染；
- 检查导入文件未发生不必要转码，视频元数据/陀螺仪解析行为与原文件一致。

## 完成标准

- iOS 三个相关入口按“用户交互”一节工作；
- 相册视频可单选和多选并能进入现有预览/队列管线；
- 文件和文件夹选择仍能访问系统“文件”App 暴露的内置及外接存储；
- 其他平台的交互和回调没有变化；
- 自动测试、iOS 编译和真机关键场景验证通过。
