## MODIFIED Requirements

### Requirement: PLUGIN_ASSET_NAMES 字面契约

`publish_pan123_release.py` 必须维护一个名为 `PLUGIN_ASSET_NAMES` 的常量，其值 SHALL 是恰好 5 个字面字符串元组：

```
GyroflowNiyien-OpenFX-windows.zip
GyroflowNiyien-Adobe-windows.aex
GyroflowNiyien-OpenFX-macos.zip
GyroflowNiyien-Adobe-macos.zip
GyroflowNiyien-OpenFX-linux.zip
```

这 5 个字面值 SHALL 同时是：
- plugin 仓（NiYien/gyroflow-plugins）GitHub release asset 文件名
- publish 脚本上传到 123 网盘 `releases/<content_tag>/plugins/` 下的文件名
- 客户端 `nle_plugins.rs::install()` 按平台拼接下载 URL 时的文件名后缀

任何环节漂移 SHALL 视为契约破坏并阻止 publish。Linux Adobe、macOS DMG 和 frei0r 产物 SHALL NOT 进入该客户端安装资产契约。

#### Scenario: publish 脚本拉 release asset
- **WHEN** publish 脚本以 release 模式从 NiYien/gyroflow-plugins 指定 tag 拉取资产
- **THEN** SHALL 按 `PLUGIN_ASSET_NAMES` 5 项字面名逐一精确匹配
- **AND** 任一项缺失 SHALL 以包含缺失文件名的错误终止 publish

#### Scenario: 客户端按平台拼 URL
- **WHEN** Windows 客户端安装 OpenFX 或 Adobe
- **THEN** SHALL 分别选择 Windows OpenFX ZIP 或 Adobe AEX
- **WHEN** macOS 客户端安装 OpenFX 或 Adobe
- **THEN** SHALL 分别选择 macOS OpenFX ZIP 或 Adobe ZIP
- **WHEN** Linux 客户端安装 OpenFX
- **THEN** SHALL 选择 `GyroflowNiyien-OpenFX-linux.zip`

#### Scenario: Linux Adobe 与其它非客户端资产不进契约
- **WHEN** plugin 仓存在 Linux Adobe、macOS DMG 或 frei0r 产物
- **THEN** `PLUGIN_ASSET_NAMES` SHALL NOT 包含这些文件
- **AND** Linux 客户端 SHALL NOT 构造 Adobe 下载 URL

### Requirement: 客户端 NLE plugin 安装路径

`src/nle_plugins.rs::get_path(typ)` SHALL 按 typ 与 OS 返回安装目标路径：

| typ | OS | 路径 |
|---|---|---|
| openfx | windows | `C:/Program Files/Common Files/OFX/Plugins/GyroflowNiyien.ofx.bundle` |
| adobe | windows | `C:/Program Files/Adobe/Common/Plug-ins/7.0/MediaCore/GyroflowNiyien-Adobe-windows.aex` |
| openfx | macos | `/Library/OFX/Plugins/GyroflowNiyien.ofx.bundle` |
| adobe | macos | `/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore/GyroflowNiyien.plugin` |
| openfx | linux | `/usr/OFX/Plugins/GyroflowNiyien.ofx.bundle` |

Linux Adobe and every unknown plugin type SHALL be unavailable and SHALL NOT fall through to macOS filenames or paths.

#### Scenario: Linux OpenFX returns standard path
- **WHEN** Linux calls `get_path("openfx")`
- **THEN** it SHALL return `/usr/OFX/Plugins/GyroflowNiyien.ofx.bundle`

#### Scenario: Linux Adobe is unavailable
- **WHEN** platform availability is resolved for Linux
- **THEN** Adobe SHALL not be offered to the caller
- **AND** no macOS Adobe path SHALL be returned

#### Scenario: Unknown plugin type is rejected
- **WHEN** a caller requests an unsupported plugin type such as `frei0r` through the NLE installer API
- **THEN** the call SHALL fail explicitly rather than selecting another platform's path

### Requirement: 客户端 detect 探测路径

`src/nle_plugins.rs::detect(typ)` SHALL detect the installed platform payload and return an empty string for not installed.

On Windows OpenFX it SHALL probe `<get_path()>/Contents/Win64/GyroflowNiyien.ofx` and read Windows ProductVersion. On macOS it SHALL parse `<get_path()>/Contents/Info.plist` for `CFBundleShortVersionString`. On Linux OpenFX it SHALL require `<get_path()>/Contents/Linux-x86-64/GyroflowNiyien.ofx` and read the explicit version file shipped in the bundle. An old `Gyroflow.ofx.bundle` SHALL not satisfy the NiYien detector.

#### Scenario: Windows OpenFX detect reads ProductVersion
- **WHEN** the Windows NiYien OpenFX binary exists
- **THEN** detect SHALL return its ProductVersion

#### Scenario: macOS OpenFX detect reads Info.plist
- **WHEN** the macOS NiYien bundle exists with a versioned Info.plist
- **THEN** detect SHALL return `CFBundleShortVersionString`

#### Scenario: Linux OpenFX detect reads explicit version
- **WHEN** the Linux x86_64 binary and version file both exist under the installed NiYien bundle
- **THEN** detect SHALL return the trimmed version string

#### Scenario: Incomplete Linux bundle is not installed
- **WHEN** the Linux bundle directory exists but its x86_64 binary or version file is missing
- **THEN** detect SHALL return an empty version

## ADDED Requirements

### Requirement: Linux NLE UI exposes Resolve OpenFX only

The Linux desktop client SHALL compile the NLE installer module and show the NLE section with only the DaVinci Resolve/OpenFX entry. It SHALL hide Adobe completely and SHALL NOT show an unavailable placeholder. Windows and macOS entries SHALL remain unchanged.

#### Scenario: Linux NLE menu contains one supported plugin type
- **WHEN** the application runs on Linux x86_64
- **THEN** the NLE section SHALL be visible
- **AND** SHALL offer OpenFX/DaVinci Resolve install or update
- **AND** SHALL NOT render an Adobe action

### Requirement: Linux OpenFX install uses constrained privilege escalation

The Linux installer SHALL validate the downloaded ZIP contract and canonicalize its extracted bundle before copying. It SHALL first attempt a normal copy to `/usr/OFX/Plugins`. If permission is denied, it SHALL invoke `pkexec` without a shell and with a fixed system destination. No download URL, ZIP member name, QML value, or arbitrary user-supplied destination SHALL become executable shell text.

If `pkexec` is missing, authentication is cancelled, or the privileged copy fails, the installer SHALL preserve the extracted directory and return a Linux-specific typed/sentinel error containing enough information for the UI to show a fixed manual install command.

#### Scenario: Writable system path installs directly
- **WHEN** `/usr/OFX/Plugins` is writable and the Linux ZIP contract is valid
- **THEN** the bundle SHALL be copied without privilege escalation
- **AND** post-install detection SHALL return the packaged version

#### Scenario: Permission denial invokes pkexec without shell
- **WHEN** direct copy fails with permission denied and `pkexec` is available
- **THEN** the installer SHALL request authorization for a fixed copy operation
- **AND** SHALL NOT execute `sh -c`, `bash -c`, or a command assembled from untrusted text

#### Scenario: Privilege escalation unavailable preserves manual fallback
- **WHEN** PolicyKit is unavailable, cancelled, or unsuccessful
- **THEN** the extracted bundle SHALL remain on disk
- **AND** the UI SHALL display its absolute path and the fixed `/usr/OFX/Plugins` destination

### Requirement: Linux installs Resolve Utility scripts in the user data directory

For Linux OpenFX installs, the three validated `ResolveScripts` sidecars SHALL be copied to `~/.local/share/DaVinciResolve/Fusion/Scripts/Utility` without root privileges. The existing legacy Gyroflow NiYien entry SHALL be removed after the replacement succeeds while unrelated Utility scripts SHALL remain untouched.

#### Scenario: Linux sidecars install without elevation
- **WHEN** a Linux OpenFX ZIP contains all three required Resolve scripts
- **THEN** they SHALL be copied to the Linux Resolve Utility directory
- **AND** the operation SHALL not invoke `pkexec`

#### Scenario: Missing sidecar blocks partial install result
- **WHEN** any required `ResolveScripts` file is absent
- **THEN** installation SHALL fail validation before reporting success
- **AND** SHALL not claim that the plugin and scripts are fully installed

### Requirement: Plugin producer publishes a versioned Linux OpenFX ZIP without Linux Adobe

The `gyroflow-plugins` Linux job SHALL build x86_64 OpenFX and package the standard Linux bundle, all three Resolve sidecars, license metadata, and an explicit version file into `GyroflowNiyien-OpenFX-linux.zip`. The Linux job SHALL NOT build or upload an Adobe artifact. Existing frei0r production MAY remain independent of the client-install asset contract.

#### Scenario: Linux plugin ZIP has exact runtime layout
- **WHEN** the Linux plugin deploy completes
- **THEN** the ZIP SHALL contain `GyroflowNiyien.ofx.bundle/Contents/Linux-x86-64/GyroflowNiyien.ofx`
- **AND** SHALL contain the explicit version file and all three `ResolveScripts` files

#### Scenario: Linux workflow does not publish Adobe
- **WHEN** the plugin release matrix runs its Linux target
- **THEN** it SHALL not invoke the Adobe deploy recipe
- **AND** SHALL not upload a `GyroflowNiyien-Adobe-linux` artifact or release asset
