# 剪映 macOS 接力验证

这是能力探针，不是可用的防抖插件。当前仅 Windows 指定剪映构建通过宿主验收；没有 Mac 现场，不知道 macOS 剪映是否实际开放第三方 OFX、真实搜索目录及 GPU 契约。客户端和正式 CI 不提供 macOS 剪映安装包。

## 先在 Mac 收集宿主事实

1. 使用单独的临时草稿，不改原片或原 `.gyroflow` 工程。记录剪映准确版本、系统、机器型号，以及是否通过 Rosetta 运行。CapCut 是另一个验证目标。
2. 执行 `python3 collect_host.py --app '/Applications/剪映专业版.app' --output host.json`，路径按实际安装位置填写。只读取指定 app，不修改签名、宿主配置或系统环境。报告中的 loader 字串仅是线索。
3. 记录界面是否有“导入插件”入口。不要把 Windows 的 `User Data/Cache/OFX` 或标准 `/Library/OFX/Plugins` 当作已确认 Mac 路径。通过导入入口选择下面构建的独立探针，日志中的 `loaded_binary` 才能证明实际加载位置。没有入口或加载日志时，先保留截图/宿主错误/签名日志，再调查 loader；不盲目复制产品包。

## 构建并导入独立能力探针

在插件仓执行 `cargo metadata --locked --format-version 1`，从 `ofx_sys` 包的 `manifest_path` 找到同目录的 `native/openfx/include`。使用正式锁定的头文件，不更换核心依赖。

```sh
bash build.sh /absolute/path/to/ofx-sys/native/openfx/include "$PWD/output"
```

构建为 arm64+x86_64 的 universal bundle。默认仅本地 ad-hoc 签名；有 Developer ID 时可在本次命令前传入 `SIGNING_IDENTITY`。这不等于已公证，也不保证宿主允许加载。脚本不安装、不启动宿主。先保存并正常退出剪映，再由用户通过实际导入功能选择 `output/GyroflowJianyingMacProbe.ofx.bundle`。

探针使用独立 ID，不改变任何现有 Gyroflow 效果。它记录 host 名称/版本、suite 属性、GPU声明和 Render flags/queue，以及参数动作序列。日志在 `~/Library/Logs/Gyroflow/jianying-ofx-probe.log`。改变数值、点击按钮并保存退出重开，保留各轮日志和界面实际显示。**探针故意不输出画面，Render 返回 unsupported；红帧或报错不能用来判断防抖核心已支持。** 本轮仅在 Windows 做头文件语法检查；Mac 原生编译和加载仍待执行。

## 分阶段验收，逐项记录

| 项目 | 当前状态 | Mac 要提供的证据 |
|---|---|---|
| 基础加载及目录 | 未验证 | 导入过程、loaded_binary、实际 loader 二进制 SHA/宿主版本 |
| 架构 | 未验证 | lipo 结果与运行中进程架构；原生 arm64、Intel x86_64、Rosetta 分别记录 |
| GPU 路径 | 未验证 | host 支持声明、实际 Render enabled/queue；不能只看系统有 Metal |
| 防抖画面/方向/时域 | 未验证 | 原生 GPU 路径实现后，非对称像素、trim、不同帧率、实际防抖画面 |
| 性能 | 未验证 | 4K 预览/导出与读回次数、实际GPU日志；CPU可出图不能算完成 |
| UI/合法写回 | 未验证 | Begin/Changed/End、按钮pulse+restore、Label实际显示及Render零写 |
| 自动加载 | 未验证 | 当前实例的可靠素材来源和代理校验；Windows私有ABI不可复用 |
| 保存退出重开 | 未验证 | 精确绑定、平滑度48、位移关闭、静态缩放均保留 |
| 签名/公证分发 | 未验证 | Developer ID、嵌套二进制签名、notarytool记录、真实下载后Gatekeeper及宿主加载 |

优先使用宿主实际提供的 Metal 资源/queue 与现有共用核心互操作。如果仅提供 OpenGL，需要先实证资源共享及高效传递；不能移植 D3D11/ANGLE 或默默退到 CPU。Windows 的 MSVC 对象偏移、`cccreator.dll` SHA 和私有 Source 链在 Mac 全部无效；没有可验证来源时保留 Browse。

Apple 的 library validation 可能阻止另一 Team ID 的动态插件。公证成功也不能替宿主增加 entitlement；不重新签名或修改剪映来绕过。参考 [Apple library validation](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.cs.disable-library-validation)、[公证与插件的宿主权限](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)、[OFX macOS 包与架构规范](https://openfx.readthedocs.io/en/main/Reference/ofxPackaging.html)。

交回 `host.json`、探针完整日志、准确宿主/系统/运行架构、导入结果与参数保存重开结果即可继续。不要上传原素材或原工程。
