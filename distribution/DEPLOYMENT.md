# NiYien 统一控制面部署与操作手册

本文档对应当前目标架构：

- `docs` 仓库：唯一控制面
- `gyroflow` 仓库：唯一产物面
- `NiYien Tool` 与 `gyroflow(niyien)`：共用 telemetry
- 控制中心：本地 Python 桌面程序

## 1. 最终架构

### `docs` 仓库负责

- `https://www.niyien.com/api/manifest`
- `https://www.niyien.com/api/telemetry`
- `https://www.niyien.com/api/telemetry-stats`
- `https://www.niyien.com/api/telemetry-rebuild`
- `stats.html`
- Vercel 环境变量

### `gyroflow` 仓库负责

- Windows / macOS / Android 安装包
- Linux 发布产物当前仍暂停，不在本次 Android APK 恢复范围内
- `lens` 数据包
  - 内含 `lens/`
  - 内含 `camera_db/`
- GitHub Release
- 国内镜像同步

### `download.niyien.com` 负责

- 国内大文件下载
- 目录结构按 tag 划分：

```text
https://download.niyien.com/releases/<tag>/...
```

## 2. telemetry 统一规则

### 产品归属

以后统一统计口径：

- `product_id = "gyroflow_niyien"`

### 来源区分

保留真实来源：

- `source_app_id = "niyien_tool"`
- `source_app_id = "gyroflow_niyien"`

### 兼容规则

旧 `NiYien Tool` 的原始 payload 继续可发，不要求客户端先升级。

当旧 Tool 未发送新字段时，`docs/api/telemetry.js` 必须自动补：

```json
{
  "source_app_id": "niyien_tool",
  "product_id": "gyroflow_niyien"
}
```

新 `gyroflow(niyien)` 发送：

```json
{
  "source_app_id": "gyroflow_niyien",
  "product_id": "gyroflow_niyien"
}
```

## 3. manifest 负责什么

`manifest` 是更新清单接口，只负责告诉客户端：

- 当前自动推送哪个应用版本
- 手动白名单有哪些版本
- `lens` 版本和地址
- SDK / 插件下载地址
- 国内 / 海外分流结果

`manifest` 固定返回：

- `app.version`
- `app.url`
- `app.changelog`
- `app.manual_versions[]`
- `lens`
- `sdk_base`
- `plugins_base`

## 4. Vercel 环境变量

`docs` 仓库 Vercel 项目必须至少配置：

- `NIYIEN_RELEASE_POLICY_JSON`
- `NIYIEN_CONTENT_RELEASE_TAG`
- `NIYIEN_LENS_VERSION`
- `NIYIEN_LENS_SHA256`
- `TELEMETRY_STATS_TOKEN`
- `TELEMETRY_REBUILD_TOKEN`
- `KV_REST_API_URL` 或 `UPSTASH_REDIS_REST_URL`
- `KV_REST_API_TOKEN` 或 `UPSTASH_REDIS_REST_TOKEN`
- `IPINFO_TOKEN`

可选：

- `deploy_hook_url`

### `NIYIEN_RELEASE_POLICY_JSON` 示例

```json
{
  "auto_version": "1.6.3",
  "versions": [
    {
      "version": "1.6.3",
      "tag": "v1.6.3",
      "channels": ["auto", "manual"],
      "changelog": "稳定版 (GitHub tag build)",
      "recommended": true
    },
    {
      "version": "1.6.3-ni.7",
      "tag": "run-12345678",
      "channels": ["manual"],
      "changelog": "小更新，仅手动可见 (GitHub Action artifact)",
      "recommended": false
    }
  ]
}
```

> 版本号格式说明：`build.rs:57-83` 生成的 canonical 有三种 —
> tag 构建用纯主版本 `1.6.3`；Action 构建用 `<major>.<minor>.<patch>-ni.<GITHUB_RUN_NUMBER>`；本地 dev 构建用 `<base>-dev.<BUILD_TIME>`。
> `control_center` 发布到 `NIYIEN_RELEASE_POLICY_JSON` 的 `version` 字段必须严格采用上述格式之一。
> 客户端 `distribution.rs::has_app_update()` 使用自定义 niyien 比较器（**不**是 SemVer 默认）：跨 base 看 (major,minor,patch)；同 base 内"裸 base < 任何带后缀 build"；同 schema 按尾部数字递增；跨 schema 时 `ni > dev`。

## 5. 首次部署

### 步骤 1：准备域名与服务

- `www.niyien.com` -> Vercel
- `download.niyien.com` -> 国内静态下载源

国内镜像根目录建议：

```text
/data/www/download.niyien.com/releases
```

### 步骤 2：完成 `docs` 仓库改造

当前会话不能直接写 `docs` 仓库，所以已在当前仓库提供模板与说明：

- [distribution/docs_control_plane_templates/README.md](C:/Users/Jhe/Desktop/github/gyroflow/distribution/docs_control_plane_templates/README.md)
- [api/manifest.js](C:/Users/Jhe/Desktop/github/gyroflow/api/manifest.js)
- [distribution/control_center.py](C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.py)

你需要在 `docs` 仓库落实：

- `api/manifest.js`
- `api/telemetry.js`
- `api/telemetry-stats.js`
- `stats.html`
- `vercel.json`

### 步骤 3：配置 GitHub Secrets

在 `gyroflow` 仓库配置：

#### macOS

- `MACOS_CERTIFICATES`
- `MACOS_CERTIFICATE_PWD`
- `MACOS_CERTIFICATE_FINGERPRINT`
- `MACOS_ACCOUNT_USER`
- `MACOS_ACCOUNT_PASS`
- `MACOS_TEAM`

#### Android

- `ANDROID_RELEASE_KEYSTORE`
- `ANDROID_RELEASE_KEYSTORE_ALIAS`
- `ANDROID_RELEASE_KEYSTORE_PASS`

tag release 的 Android 构建必须三者全部配置，且 `ANDROID_RELEASE_KEYSTORE`
必须能解码成有效 keystore 文件；缺失时构建应直接失败，不能上传 debug APK。

#### 国内镜像同步

- `NIYIEN_MIRROR_HOST`
- `NIYIEN_MIRROR_USER`
- `NIYIEN_MIRROR_KEY`
- `NIYIEN_MIRROR_PATH`

推荐：

```text
NIYIEN_MIRROR_PATH=/data/www/download.niyien.com/releases
```

### 步骤 4：配置 Vercel 环境变量

在 `docs` 仓库对应的 Vercel 项目里填好第 4 节所有环境变量。

### 步骤 5：发布应用版本

应用 workflow：

- [C:/Users/Jhe/Desktop/github/gyroflow/.github/workflows/release.yml](C:/Users/Jhe/Desktop/github/gyroflow/.github/workflows/release.yml)

发布方法：

1. 先 `workflow_dispatch`
2. 再打应用 tag

示例：

```text
v1.6.3
```

（tag 必须是裸 `vX.Y.Z`，任何带后缀（`-niyien.1`、`-beta1`、`-rc1` 等）的 tag 都会被 `release.yml::validate-tag` 直接拒掉。`-ni.<RUN_NUMBER>` 是 build.rs 对非 tag 构建自动追加的 schema，用户无需也不应该在 tag 名里手动写。需要灰度时走 workflow_dispatch + control_center 在 `manual` channel 发 `-ni.<N>`。）

产物：

- `gyroflow-niyien-windows64.zip`
- `gyroflow-niyien-mac-universal.dmg`
- `gyroflow-niyien.apk`

其中 Android APK 来源于 release workflow 的 Android target，并由
`just android deploy` 生成到：

```text
_deployment/_binaries/gyroflow-niyien.apk
```

Linux 产物仍暂停；如果后续恢复 Linux，应单独更新 workflow、发布清单和验收步骤。

### 步骤 6：发布数据版本

数据 workflow：

- [C:/Users/Jhe/Desktop/github/gyroflow/.github/workflows/data-release.yml](C:/Users/Jhe/Desktop/github/gyroflow/.github/workflows/data-release.yml)

原始数据目录：

- [C:/Users/Jhe/Desktop/github/gyroflow/distribution/data/lens/](C:/Users/Jhe/Desktop/github/gyroflow/distribution/data/lens/)

其中建议结构：

- `distribution/data/lens/lens/`
- `distribution/data/lens/camera_db/`

发布方法：

1. 先本地验证

```powershell
python _scripts/package_lens.py --version 1
```

2. 再打数据 tag，例如：

```text
data-v20260411.1
```

3. 发布完成后，把对应元信息写入 Vercel：

- `NIYIEN_CONTENT_RELEASE_TAG`
- `NIYIEN_LENS_VERSION`
- `NIYIEN_LENS_SHA256`

## 6. 控制中心

控制中心程序（pywebview 新版）：

- [distribution/control_center/control_center.py](C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center/control_center.py)

配置示例：

- [distribution/control_center/control_center.example.json](C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center/control_center.example.json)

本地实际配置文件：

- `distribution/control_center/control_center.config.json`

建议安装依赖：

```powershell
pip install requests pywebview
```

运行（pywebview 新版 UI）：

```powershell
python distribution/control_center/control_center.py
```

需要 DevTools 调试时设环境变量 `CONTROL_CENTER_DEBUG=1` 后再启动。

旧版 Tkinter UI 作为备份保留在 `distribution/control_center/_legacy/control_center_legacy_tkinter.py`，可通过 `python distribution/control_center/_legacy/control_center_legacy_tkinter.py` 启动（仅作回滚用途）。

### 控制中心分区

- 应用发布
- 数据资源发布
- 下载与路由
- 统计与观测
- 高级设置

### 控制中心支持的动作

#### 应用发布

- 发布新应用，但不推送
- 发布并立即推送
- 开始推送已发布版本
- 回滚自动推送版本
- 隐藏某个版本

#### 数据资源发布

- 从 GitHub Release 读取数据元信息
- 发布新数据资源
- 切换当前数据资源版本

#### 下载与路由

- 预览某国家 + 平台的 manifest 最终返回结果

#### 统计与观测

- 获取 telemetry-stats JSON
- 打开 stats.html
- 触发 telemetry rebuild

## 7. 后续操作手册

### 场景 A：发布新应用，但不推送

1. 在 `gyroflow` 仓库发布新应用 tag
2. 等 GitHub Release 与国内镜像同步完成
3. 打开控制中心
4. 在“应用发布”页选择这个 release
5. 点击 `发布新应用，但不推送`

结果：

- 新版本进入 `manual_versions`
- `auto_version` 不变
- 用户自动更新不提示
- 用户手动版本列表可见

### 场景 B：发布并立即推送

1. 在 `gyroflow` 仓库发布新应用 tag
2. 控制中心选择该版本
3. 点击 `发布并立即推送`

结果：

- 该版本进入白名单
- 同时成为 `auto_version`
- 自动更新开始提示

### 场景 C：开始推送已发布版本

1. 该版本必须已经存在于白名单
2. 在控制中心选择该版本
3. 点击 `开始推送已发布版本`

结果：

- 只切换 `auto_version`
- 不需要重新打包客户端

### 场景 D：回滚自动推送版本

1. 控制中心选择目标稳定版
2. 点击 `回滚自动推送版本`

结果：

- `auto_version` 切回旧版
- 已在新版上的用户不会收到降级提示

### 场景 E：隐藏某个版本

1. 选择该版本
2. 点击 `隐藏某个版本`

结果：

- 从白名单移除
- 如果它原本是自动推送版本，则自动版本切回列表中的第一个版本

### 场景 F：发布新数据资源

1. 在 `gyroflow` 仓库发布数据 tag
2. 控制中心填写：
   - `内容 Release Tag`
   - `lens` 版本 / sha256
3. 点击 `发布新数据资源`

结果：

- 更新数据相关 Vercel 环境变量
- 客户端下次启动按新 manifest 拉数据

## 8. 验收清单

### 客户端

你本地执行：

```text
just run
```

确认：

1. 启动会请求 `/api/manifest`
2. 自动更新只提示 `auto_version`
3. 高级设置里能查看手动白名单版本
4. 下载按钮打开 manifest 返回的具体 URL
5. `lens` 包可以更新，并且其中的 `camera_db` 会跟随一起更新
6. `/api/manifest?platform=android` 返回 `app.packages.android.package_url`
   且 `app.url` 与该 URL 一致，不返回 Android `installer_url`

### 发布产物

确认：

1. `workflow_dispatch` 后出现 macOS、Windows、Android 三类 artifacts
2. Android artifact 名为 `gyroflow-niyien-android`，下载后包含 `gyroflow-niyien.apk`
3. tag release 的 GitHub Release assets 包含裸文件 `gyroflow-niyien.apk`
4. 控制中心 / 123 inventory 检查 Android 时使用远端 wrapper 名
   `gyroflow-niyien-android.zip`，不是裸 `gyroflow-niyien.apk`

### telemetry

确认：

1. 旧 `NiYien Tool` 原样上报仍然 200
2. 新 `gyroflow(niyien)` 上报带：
   - `source_app_id`
   - `product_id`
3. stats 默认按 `product_id=gyroflow_niyien` 聚合
4. 可按 `source_app_id` 拆分来源

## 9. 重要说明

- 当前会话不能直接写 `C:\Users\Jhe\Desktop\github\docs`
- 所以 `docs` 仓库的实际 API 落地，需要你在 `docs` 仓库中应用模板和规则
- 当前仓库已经提供：
  - 客户端对接逻辑
  - 控制中心程序
  - `docs` 控制面改造说明
