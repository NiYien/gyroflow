# NiYien Control Center 使用与配置指南

本文档整理了三部分内容：

1. 从 0 开始的照着点操作版
2. `control_center.config.json` 的逐项填写说明
3. GitHub Token 权限选择建议

适用对象：

- 需要使用本地 `Control Center` 管理 NiYien 发布流程的人
- 需要配置 GitHub / Vercel / 123 云盘 / 本地控制中心的人

当前版本的重要变化：

- 123 上传已经完全改为本地 `Control Center` 执行
- GitHub Actions 不再负责上传 123
- `PAN123_*` 只需要保存在本地 `control_center.config.json`
- “下次发版默认资源源”也改为保存在本地配置，不再写 GitHub Actions Variables

---

## 1. 先说结论

以后你日常主要只做三件事：

1. 选择资源源（如果这次要换）
2. 在控制中心里创建并推送 Tag
3. 选择是否推送

现在应用发布新增了第二条来源：

- `GitHub Release`
  - 正式长期版本
  - 继续走 Tag 发布
- `Action 构建`
  - 无 Tag 的临时版本
  - 先在控制中心点 `执行 Action 编译（不创建 Tag）`
  - 等构建完成后，再在 `应用发布` 里按正常按钮发布或推送
  - 这类版本默认按临时版本处理，GitHub artifact 最长保留 90 天

真正会用到的地方只有这 4 个：

1. `123 云盘`
2. `GitHub`
3. `Vercel`
4. `Control Center`

在控制中心里，你最常用的页面是：

- `应用发布`
- `资源编排`

---

## 2. 从 0 开始的照着点操作版

### 2.1 第一次上线前，只做一次

#### 第 1 步：123 云盘里准备好

1. 登录 123 开放平台。
2. 创建 OpenAPI 应用。
3. 记下：
   - `clientID`
   - `clientSecret`
4. 在 123 里启用直链空间。
5. 在 123 里新建一个专门存发布文件的目录，建议叫：
   - `releases`
6. 记下这个目录的 ID。

你最后要拿到这 3 个值：

- `PAN123_CLIENT_ID`
- `PAN123_CLIENT_SECRET`
- `PAN123_RELEASES_ROOT_ID`

#### 第 2 步：Vercel 里给 docs 项目填环境变量

去 `docs` 对应的 Vercel 项目：

1. 打开 Vercel
2. 进入 `docs` 项目
3. 点 `Settings`
4. 点 `Environment Variables`
5. 检查线上运行时变量是否齐全
6. 填完后执行一次重新部署

#### 第 3 步：把代码推上去

这一步第一次接入时必须确认：

1. 推 `gyroflow`
2. 推 `docs`

也就是：

- `gyroflow` 里的 workflow、123 发布脚本、控制中心改动
- `docs` 里的 manifest、download API、123 解析逻辑改动

#### 第 4 步：本地 Control Center 填配置

本地配置文件路径：

- `C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.config.json`

如果没有，可以参考模板：

- `C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.example.json`

填好之后运行：

```powershell
python C:\Users\Jhe\Desktop\github\gyroflow\distribution\control_center.py
```

本地配置里需要包含：

- `pan123_client_id`
- `pan123_client_secret`
- `pan123_releases_root_id`

现在这些 123 凭据只在本地使用，不再要求同步到 GitHub Actions。

---

### 2.2 每次发版前，如果要换资源版本

这一步都在控制中心的 `资源编排` 页面做。

你会看到这些字段：

- `Lens/CameraDB Tag`
- `Plugin 来源模式`
- `Plugin Release Tag`
- `Plugin Artifact 名称`
- `发版用 SDK 下载源 (NIYIEN_SDK_BASE)`

你也会看到一个 `来源切换`：

- `上次使用`
  - 把本地上一次保存的默认值重新读回来
- `最新推荐`
  - 自动查询最新 Lens/CameraDB Release
  - 自动查询最新 Plugin Release / Artifact
  - 自动填入默认 SDK 下载源
- `当前自定义`
  - 表示你正在手动调整当前表单

这里最容易混的是 SDK：

- `NIYIEN_SDK_BASE`
  - 在 `资源编排` 里设置
  - 用于下一次应用发版时，发布脚本去哪里下载 SDK
  - 也会写进这次发版产出的 release summary
- `NIYIEN_GLOBAL_SDK_BASE`
  - 不在这里设置
  - 它是线上全局运行时分发用的环境变量
  - 主要给 manifest / 全球访问链路参考

所以如果你要改“下一次发版会用哪个 SDK 下载源”，要改的是：

- `发版用 SDK 下载源 (NIYIEN_SDK_BASE)`

#### 方式 A：直接用最新版本

1. 点 `使用最新推荐`
2. 检查自动填出来的：
   - `Lens/CameraDB Tag`
   - `Plugin 来源模式`
   - `Plugin Release Tag`
   - `发版用 SDK 下载源 (NIYIEN_SDK_BASE)`
3. 如果不用改，直接点 `保存为下次发版默认值`

#### 方式 B：手动指定版本

1. 先点 `使用上次默认源` 或 `使用最新推荐` 作为起点
2. 手动修改 `Lens/CameraDB Tag`
3. 选择 `Plugin 来源模式`
4. 如果是 `release`，填写 `Plugin Release Tag`
5. 如果是 `artifact`，填写 `Plugin Artifact 名称`
6. `Plugin Artifact 名称` 可以留空
7. 留空时会自动取插件仓库默认分支最近成功的 Action run
8. 手动填写 `发版用 SDK 下载源 (NIYIEN_SDK_BASE)`
9. 点 `保存为下次发版默认值`

结果：

- 这些值会保存到本地控制中心配置
- 下次你在本地发布中心执行 CN 发布时，会自动使用这些值
- 其中 SDK 用的是 `NIYIEN_SDK_BASE`，不是 `NIYIEN_GLOBAL_SDK_BASE`

注意：

- `Lens` 和 `CameraDB` 当前共用同一个 Tag
- `SDK` 不是单独 Tag，而是一个基础地址
- `资源编排` 页面里的 SDK 字段，指的是“发版脚本下载 SDK 的来源”
- `Plugin` 可以走 `Release` 或 `Action artifact`
- `Plugin Artifact 名称` 支持留空，留空表示自动取最新成功 run

---

### 2.3 在控制中心里创建并推送 Tag

现在不需要切回终端手动执行 `git tag` 了。

你可以直接在控制中心里做：

位置：

- 首页 `操作清单`
  - `创建并推送 Tag`
- 或 `应用发布`
  - `版本信息` 区域里的 `创建并推送 Tag`

使用方法：

#### 方式 A：先填版本号

如果你填的是：

- `版本号 = 1.6.3-niyien.1`

那控制中心会自动生成：

- `Tag = v1.6.3-niyien.1`

然后直接创建并推送。

#### 方式 B：直接填 Tag

如果你自己手动填：

- `Tag = v1.6.3-niyien.1`

那控制中心就会直接按这个 Tag 创建并推送。

控制中心会自动做这些检查：

- 当前工作区所在分支
- 本地是否已经有这个 Tag
- 远端是否已经有这个 Tag
- 当前工作区是否还有未提交改动

确认后会自动执行：

1. 创建本地 Tag
2. 推送到远端

---

### 2.4 第一次正式发版怎么做

1. 推送代码
2. 打开控制中心
3. 进入 `应用发布`
4. 填：
   - `版本号`
   - 或直接填 `Tag`
5. 点 `创建并推送 Tag`
6. 等 GitHub Actions 跑完
7. 点 `刷新 Releases`
8. 选中刚发布的版本
9. 如果要核对内容版本，去 `资源编排`
10. 点 `从选中应用 Release 读取`
11. 确认读到了：
    - `内容 Release Tag`
    - `lens 版本`
    - `lens sha256`
12. 回到 `应用发布`
13. 根据目标点一个按钮：
    - `发布新应用，但不推送`
    - `发布并立即推送`

---

### 2.5 以后每次正常发版怎么做

1. 如果这次要换资源版本，先去 `资源编排`
2. 改好：
   - `Lens/CameraDB Tag`
   - `Plugin 来源模式`
   - `Plugin Release Tag` 或 `Plugin Artifact 名称`
   - `SDK 基础地址`
3. 点 `保存为下次发版默认源`
4. 提交代码并推送
5. 去 `应用发布`
6. 填：
   - `版本号`
   - 或 `Tag`
7. 点 `创建并推送 Tag`
8. 等 GitHub Actions 完成
9. 点 `刷新 Releases`
10. 选中刚发布的版本
11. 点一个按钮：
    - `发布新应用，但不推送`
    - `发布并立即推送`

---

### 2.6 如果你想“发布但不推送更新”

操作：

1. 在控制中心里创建并推送 Tag
2. 等 GitHub Actions 完成
3. 打开控制中心
4. 进入 `应用发布`
5. 选中版本
6. 点 `发布新应用，但不推送`

效果：

- 新版本上线
- 用户手动版本列表里能看到
- 已安装用户不会收到自动更新提示

---

### 2.7 如果你想“发布并推送更新”

操作：

1. 在控制中心里创建并推送 Tag
2. 等 GitHub Actions 完成
3. 打开控制中心
4. 进入 `应用发布`
5. 选中版本
6. 点 `发布并立即推送`

效果：

- 新版本上线
- 用户手动版本列表里能看到
- 已安装用户会收到自动更新提示

---

### 2.8 如果你之前发了版本，后来才想开始推送

操作：

1. 打开控制中心
2. 进入 `应用发布`
3. 选中已经发布过的版本
4. 点 `开始推送已发布版本`

效果：

- 不重新构建
- 不重新上传
- 只切换推送状态

---

### 2.9 如果新版本要回滚

操作：

1. 打开控制中心
2. 进入 `应用发布`
3. 选中一个旧的稳定版本
4. 点 `回滚自动推送版本`

效果：

- 后续用户看到的推荐更新切回旧版本
- 已安装新版本的用户不会被强制降级

---

### 2.10 如果某个版本不想再让用户看到

操作：

1. 打开控制中心
2. 进入 `应用发布`
3. 选中目标版本
4. 点 `隐藏某个版本`

效果：

- 该版本不会再出现在手动版本列表里
- 如果它是当前推送版本，会自动切回别的版本

---

### 2.11 如果你想确认中国区下载是否正常

操作：

1. 打开控制中心
2. 进入 `下载与路由`
3. `国家代码` 填 `CN`
4. `平台` 选 `windows`
5. 点 `预览 manifest 返回结果`

重点确认：

- `app.url` 已经是你自己的 `/api/download/...`
- `lens.url` 已经是你自己的 `/api/download/...`
- `sdk_base`、`plugins_base` 也是你自己的下载入口

---

## 3. control_center.config.json 每一行怎么填

文件路径：

- `C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.config.json`

参考模板：

- `C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.example.json`

---

### `vercel_token`

作用：

- 控制中心用它读写 Vercel 项目的环境变量

从哪里来：

- Vercel 后台生成的个人 Token

怎么拿：

1. 登录 Vercel
2. 打开账号设置
3. 找 `Tokens`
4. 新建一个 Token

怎么填：

```json
"vercel_token": "你的 Vercel Token"
```

---

### `vercel_project_id_or_name`

作用：

- 告诉控制中心你要操作哪个 Vercel 项目

从哪里来：

- 你 `docs` 项目在 Vercel 里的项目名，或项目 ID

建议：

- 优先填项目名

怎么填：

```json
"vercel_project_id_or_name": "你的 docs 项目名"
```

或者：

```json
"vercel_project_id_or_name": "prj_xxxxxxxxx"
```

---

### `vercel_team_id`

作用：

- 如果你的 Vercel 项目属于 team，需要这个值

从哪里来：

- Vercel team 设置页

怎么填：

团队项目：

```json
"vercel_team_id": "team_xxxxxxxxx"
```

个人项目：

```json
"vercel_team_id": ""
```

---

### `github_token`

作用：

- 控制中心用它读取 GitHub Releases
- 也用它读写 GitHub Actions Variables

从哪里来：

- GitHub Personal Access Token

怎么填：

```json
"github_token": "你的 GitHub Token"
```

---

### `github_owner`

作用：

- 控制中心默认访问哪个 GitHub 仓库拥有者

你现在应该填：

```json
"github_owner": "NiYien"
```

---

### `github_repo`

作用：

- 控制中心默认访问哪个 GitHub 仓库

你现在应该填：

```json
"github_repo": "gyroflow"
```

注意：

- 这里填的是 **仓库名**
- 不是分支名
- `niyien` 如果是分支，不需要单独配置在这里

---

### `lens_data_owner`

作用：

- 控制中心去哪个仓库读取 `Lens/CameraDB` 的 release tag

你现在应该填：

```json
"lens_data_owner": "NiYien"
```

---

### `lens_data_repo`

作用：

- 控制中心去哪个仓库读取 `Lens/CameraDB` 的最新 release

你现在应该填：

```json
"lens_data_repo": "niyien-lens-data"
```

---

### `plugins_owner`

作用：

- 控制中心去哪个仓库读取 plugin release

你现在应该填：

```json
"plugins_owner": "gyroflow"
```

---

### `plugins_repo`

作用：

- 控制中心去哪个仓库读取 plugin release

你现在应该填：

```json
"plugins_repo": "gyroflow-plugins"
```

---

### `telemetry_base_url`

作用：

- 控制中心访问统计和控制面 API 的基础地址
- 也用它拼下载 API 基础地址

你现在应该填：

```json
"telemetry_base_url": "https://www.niyien.com"
```

注意：

- 不要加最后的 `/`

---

### `telemetry_stats_token`

作用：

- 控制中心请求 `/api/telemetry-stats` 时带的认证 token

从哪里来：

- Vercel 里 `docs` 项目配置的 `TELEMETRY_STATS_TOKEN`

怎么填：

如果已经配置了，就填同一个值：

```json
"telemetry_stats_token": "和 Vercel 里的 TELEMETRY_STATS_TOKEN 一样"
```

如果暂时不用统计页，可以先留空：

```json
"telemetry_stats_token": ""
```

---

### `telemetry_rebuild_token`

作用：

- 控制中心调用 `/api/telemetry-rebuild` 时的认证 token

从哪里来：

- Vercel 里 `docs` 项目配置的 `TELEMETRY_REBUILD_TOKEN`

怎么填：

```json
"telemetry_rebuild_token": "和 Vercel 里的 TELEMETRY_REBUILD_TOKEN 一样"
```

如果暂时不用 rebuild，也可以先留空：

```json
"telemetry_rebuild_token": ""
```

---

### `deploy_hook_url`

作用：

- 当控制中心改完 Vercel env 后，可以顺手触发一次 redeploy

从哪里来：

- Vercel 项目的 Deploy Hook

怎么拿：

1. 打开 Vercel 项目
2. Settings
3. 找 Deploy Hooks
4. 新建一个 hook
5. 拿到 URL

怎么填：

```json
"deploy_hook_url": "https://api.vercel.com/v1/integrations/deploy/..."
```

如果不想自动 redeploy，可以先留空：

```json
"deploy_hook_url": ""
```

---

### `distribution_config_path`

作用：

- 告诉控制中心读取哪个本地分发配置文件

你现在应该填：

```json
"distribution_config_path": "distribution/niyien.toml"
```

一般不用改。

---

### `git_remote`

作用：

- 控制中心创建并推送 Tag 时，默认推到哪个远端

你现在一般应该填：

```json
"git_remote": "origin"
```

如果你的正式发布远端不是 `origin`，再改它。

---

### `repo_workdir`

作用：

- 控制中心执行 `git tag` / `git push` 时，在哪个本地仓库目录执行

你现在一般应该填：

```json
"repo_workdir": "C:/Users/Jhe/Desktop/github/gyroflow"
```

如果控制中心脚本就放在这个仓库里，也可以不显式写，但推荐写上。

---

## 4. 推荐的完整填写模板

你可以先这样填：

```json
{
  "vercel_token": "你的_Vercel_Token",
  "vercel_project_id_or_name": "你的_docs_Vercel项目名或ID",
  "vercel_team_id": "",
  "github_token": "你的_GitHub_Token",
  "github_owner": "NiYien",
  "github_repo": "gyroflow",
  "lens_data_owner": "NiYien",
  "lens_data_repo": "niyien-lens-data",
  "plugins_owner": "gyroflow",
  "plugins_repo": "gyroflow-plugins",
  "telemetry_base_url": "https://www.niyien.com",
  "telemetry_stats_token": "",
  "telemetry_rebuild_token": "",
  "deploy_hook_url": "",
  "distribution_config_path": "distribution/niyien.toml",
  "git_remote": "origin",
  "repo_workdir": "C:/Users/Jhe/Desktop/github/gyroflow"
}
```

---

## 5. GitHub Token 应该怎么选权限

GitHub 现在主要有两种 PAT：

1. `classic PAT`
2. `fine-grained PAT`

---

### 5.1 `classic PAT` 和 `fine-grained PAT` 的区别

#### `classic PAT`

特点：

- 老方案
- 权限是大块打包
- 配置简单
- 权限更粗

适合：

- 想快点配好
- 不想研究太多权限细节

#### `fine-grained PAT`

特点：

- 新方案
- 可以限制到具体仓库
- 可以限制到具体权限
- 更安全
- 配置更细

适合：

- 希望控制中心只拿到 `gyroflow` 这个仓库的必要权限
- 希望 token 更安全

结论：

- **省事**：`classic PAT`
- **更安全**：`fine-grained PAT`

如果你愿意花几分钟多配置一下，推荐：

- **优先用 `fine-grained PAT`**

---

### 5.2 如果你用 `classic PAT`

最简单的选法：

只勾：

- `repo`

不要勾：

- `workflow`
- `admin:*`
- `delete:*`
- `gist`
- `notifications`
- `user`
- 其他都先不要勾

原因：

- 控制中心主要要做：
  - 读 Release
  - 读写 Actions Variables
  - 在本地仓库里创建并推送 Tag（这部分不靠 GitHub API）
- 对 `classic PAT` 来说，`repo` 已经够用

---

### 5.3 如果你用 `fine-grained PAT`

创建时这样选：

#### `Resource owner`

选：

- `NiYien`

#### `Repository access`

选：

- `Only select repositories`

然后只勾：

- `gyroflow`

#### Repository permissions

至少建议开这三个：

- `Actions` -> `Read and write`
- `Contents` -> `Read and write`
- `Variables` -> `Read and write`

说明：

- `Actions`
  - 控制中心会读写 GitHub Actions Variables 相关配置
- `Contents`
  - 控制中心要读取 Releases
  - 如果你未来还要扩展更多仓库读写行为，`Read and write` 更稳妥
- `Variables`
  - 如果没有这个权限，控制中心会出现：
    - GitHub 变量读不到
    - 默认资源源无法读取/保存

如果你页面里没有 `Variables` 这一项，说明当前 GitHub 权限模型可能把这类能力归在别的仓库权限里；但以控制中心目前实际联调经验，**只给 `Actions` 而不给变量相关权限是不够的**。

---

### 5.4 `github_repo` 要不要填分支名

不要。

例如你现在这个字段应该还是：

```json
"github_repo": "gyroflow"
```

说明：

- `github_repo` 填的是仓库名
- 不是 branch 名
- 如果你说的 `niyien` 是一个分支，不需要单独在这里配置

---

## 6. 最短结论

### 最短操作版

首次配置：

1. 123 拿 3 个值
2. GitHub 填 3 个 Secrets
3. Vercel 填 3 个 Env
4. 本地 `control_center.config.json` 填配置

每次发版：

1. 如果要换资源，先去 `资源编排`
2. 设置：
   - `Lens/CameraDB Tag`
   - `Plugin 来源模式`
   - `Plugin Release Tag` 或 `Plugin Artifact 名称`
   - `SDK 基础地址`
3. 点 `保存为下次发版默认源`
4. 去 `应用发布`
5. 填：
   - `版本号`
   - 或 `Tag`
6. 点 `创建并推送 Tag`
7. 等 GitHub Actions 完成
8. 点：
   - `发布新应用，但不推送`
   - 或 `发布并立即推送`

### GitHub Token 最短结论

- 想简单：`classic PAT`，只勾 `repo`
- 想更安全：`fine-grained PAT`
  - `Resource owner`：`NiYien`
  - `Only select repositories`：`gyroflow`
  - 权限建议开：
    - `Actions: Read and write`
    - `Contents: Read and write`
    - `Variables: Read and write`
6. 等 GitHub Actions 完成
7. 打开控制中心
8. 进入 `应用发布`
9. 点 `刷新 Releases`
10. 选中刚发布的版本
11. 点一个按钮：
    - `发布新应用，但不推送`
    - `发布并立即推送`

---

### 2.5 如果你想“发布但不推送更新”

操作：

1. 正常打应用 tag
2. 等 GitHub Actions 完成
3. 打开控制中心
4. 进入 `应用发布`
5. 选中版本
6. 点 `发布新应用，但不推送`

效果：

- 新版本上线
- 用户手动版本列表里能看到
- 已安装用户不会收到自动更新提示

---

### 2.6 如果你想“发布并推送更新”

操作：

1. 正常打应用 tag
2. 等 GitHub Actions 完成
3. 打开控制中心
4. 进入 `应用发布`
5. 选中版本
6. 点 `发布并立即推送`

效果：

- 新版本上线
- 用户手动版本列表里能看到
- 已安装用户会收到自动更新提示

---

### 2.7 如果你之前发了版本，后来才想开始推送

操作：

1. 打开控制中心
2. 进入 `应用发布`
3. 选中已经发布过的版本
4. 点 `开始推送已发布版本`

效果：

- 不重新构建
- 不重新上传
- 只切换推送状态

---

### 2.8 如果新版本要回滚

操作：

1. 打开控制中心
2. 进入 `应用发布`
3. 选中一个旧的稳定版本
4. 点 `回滚自动推送版本`

效果：

- 后续用户看到的推荐更新切回旧版本
- 已安装新版本的用户不会被强制降级

---

### 2.9 如果某个版本不想再让用户看到

操作：

1. 打开控制中心
2. 进入 `应用发布`
3. 选中目标版本
4. 点 `隐藏某个版本`

效果：

- 该版本不会再出现在手动版本列表里
- 如果它是当前推送版本，会自动切回别的版本

---

### 2.10 如果你想确认中国区下载是否正常

操作：

1. 打开控制中心
2. 进入 `下载与路由`
3. `国家代码` 填 `CN`
4. `平台` 选 `windows`
5. 点 `预览 manifest 返回结果`

重点确认：

- `app.url` 已经是你自己的 `/api/download/...`
- `lens.url` 已经是你自己的 `/api/download/...`
- `sdk_base`、`plugins_base` 也是你自己的下载入口

---

## 3. control_center.config.json 每一行怎么填

文件路径：

- `C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.config.json`

参考模板：

- `C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.example.json`

---

### `vercel_token`

作用：

- 控制中心用它读写 Vercel 项目的环境变量

从哪里来：

- Vercel 后台生成的个人 Token

怎么拿：

1. 登录 Vercel
2. 打开账号设置
3. 找 `Tokens`
4. 新建一个 Token

怎么填：

```json
"vercel_token": "你的 Vercel Token"
```

---

### `vercel_project_id_or_name`

作用：

- 告诉控制中心你要操作哪个 Vercel 项目

从哪里来：

- 你 `docs` 项目在 Vercel 里的项目名，或项目 ID

建议：

- 优先填项目名

怎么填：

```json
"vercel_project_id_or_name": "你的 docs 项目名"
```

或者：

```json
"vercel_project_id_or_name": "prj_xxxxxxxxx"
```

---

### `vercel_team_id`

作用：

- 如果你的 Vercel 项目属于 team，需要这个值

从哪里来：

- Vercel team 设置页

怎么填：

团队项目：

```json
"vercel_team_id": "team_xxxxxxxxx"
```

个人项目：

```json
"vercel_team_id": ""
```

---

### `github_token`

作用：

- 控制中心用它读取 GitHub Releases
- 也用它读写 GitHub Actions Variables

从哪里来：

- GitHub Personal Access Token

怎么填：

```json
"github_token": "你的 GitHub Token"
```

---

### `github_owner`

作用：

- 控制中心默认访问哪个 GitHub 仓库拥有者

你现在应该填：

```json
"github_owner": "NiYien"
```

---

### `github_repo`

作用：

- 控制中心默认访问哪个 GitHub 仓库

你现在应该填：

```json
"github_repo": "gyroflow"
```

注意：

- 这里填的是 **仓库名**
- 不是分支名
- `niyien` 如果是分支，不需要单独配置在这里

---

### `lens_data_owner`

作用：

- 控制中心去哪个仓库读取 `Lens/CameraDB` 的 release tag

你现在应该填：

```json
"lens_data_owner": "NiYien"
```

---

### `lens_data_repo`

作用：

- 控制中心去哪个仓库读取 `Lens/CameraDB` 的最新 release

你现在应该填：

```json
"lens_data_repo": "niyien-lens-data"
```

---

### `plugins_owner`

作用：

- 控制中心去哪个仓库读取 plugin release

你现在应该填：

```json
"plugins_owner": "gyroflow"
```

---

### `plugins_repo`

作用：

- 控制中心去哪个仓库读取 plugin release

你现在应该填：

```json
"plugins_repo": "gyroflow-plugins"
```

---

### `telemetry_base_url`

作用：

- 控制中心访问统计和控制面 API 的基础地址
- 也用它拼下载 API 基础地址

你现在应该填：

```json
"telemetry_base_url": "https://www.niyien.com"
```

注意：

- 不要加最后的 `/`

---

### `telemetry_stats_token`

作用：

- 控制中心请求 `/api/telemetry-stats` 时带的认证 token

从哪里来：

- Vercel 里 `docs` 项目配置的 `TELEMETRY_STATS_TOKEN`

怎么填：

如果已经配置了，就填同一个值：

```json
"telemetry_stats_token": "和 Vercel 里的 TELEMETRY_STATS_TOKEN 一样"
```

如果暂时不用统计页，可以先留空：

```json
"telemetry_stats_token": ""
```

---

### `telemetry_rebuild_token`

作用：

- 控制中心调用 `/api/telemetry-rebuild` 时的认证 token

从哪里来：

- Vercel 里 `docs` 项目配置的 `TELEMETRY_REBUILD_TOKEN`

怎么填：

```json
"telemetry_rebuild_token": "和 Vercel 里的 TELEMETRY_REBUILD_TOKEN 一样"
```

如果暂时不用 rebuild，也可以先留空：

```json
"telemetry_rebuild_token": ""
```

---

### `deploy_hook_url`

作用：

- 当控制中心改完 Vercel env 后，可以顺手触发一次 redeploy

从哪里来：

- Vercel 项目的 Deploy Hook

怎么拿：

1. 打开 Vercel 项目
2. Settings
3. 找 Deploy Hooks
4. 新建一个 hook
5. 拿到 URL

怎么填：

```json
"deploy_hook_url": "https://api.vercel.com/v1/integrations/deploy/..."
```

如果不想自动 redeploy，可以先留空：

```json
"deploy_hook_url": ""
```

---

### `distribution_config_path`

作用：

- 告诉控制中心读取哪个本地分发配置文件

你现在应该填：

```json
"distribution_config_path": "distribution/niyien.toml"
```

一般不用改。

---

## 4. 推荐的完整填写模板

你可以先这样填：

```json
{
  "vercel_token": "你的_Vercel_Token",
  "vercel_project_id_or_name": "你的_docs_Vercel项目名或ID",
  "vercel_team_id": "",
  "github_token": "你的_GitHub_Token",
  "github_owner": "NiYien",
  "github_repo": "gyroflow",
  "lens_data_owner": "NiYien",
  "lens_data_repo": "niyien-lens-data",
  "plugins_owner": "gyroflow",
  "plugins_repo": "gyroflow-plugins",
  "telemetry_base_url": "https://www.niyien.com",
  "telemetry_stats_token": "",
  "telemetry_rebuild_token": "",
  "deploy_hook_url": "",
  "distribution_config_path": "distribution/niyien.toml"
}
```

---

## 5. GitHub Token 应该怎么选权限

GitHub 现在主要有两种 PAT：

1. `classic PAT`
2. `fine-grained PAT`

---

### 5.1 `classic PAT` 和 `fine-grained PAT` 的区别

#### `classic PAT`

特点：

- 老方案
- 权限是大块打包
- 配置简单
- 权限更粗

适合：

- 想快点配好
- 不想研究太多权限细节

#### `fine-grained PAT`

特点：

- 新方案
- 可以限制到具体仓库
- 可以限制到具体权限
- 更安全
- 配置更细

适合：

- 希望控制中心只拿到 `gyroflow` 这个仓库的必要权限
- 希望 token 更安全

结论：

- **省事**：`classic PAT`
- **更安全**：`fine-grained PAT`

如果你愿意花几分钟多配置一下，推荐：

- **优先用 `fine-grained PAT`**

---

### 5.2 如果你用 `classic PAT`

最简单的选法：

只勾：

- `repo`

不要勾：

- `workflow`
- `admin:*`
- `delete:*`
- `gist`
- `notifications`
- `user`
- 其他都先不要勾

原因：

- 控制中心主要要做：
  - 读 Release
  - 读写 Actions Variables
- 对 `classic PAT` 来说，`repo` 已经够用

---

### 5.3 如果你用 `fine-grained PAT`

创建时这样选：

#### `Resource owner`

选：

- `NiYien`

#### `Repository access`

选：

- `Only select repositories`

然后只勾：

- `gyroflow`

#### Repository permissions

只建议开这两个：

- `Actions` -> `Read and write`
- `Contents` -> `Read-only`

其他都不要开。

---

### 5.4 `github_repo` 要不要填分支名

不要。

例如你现在这个字段应该还是：

```json
"github_repo": "gyroflow"
```

说明：

- `github_repo` 填的是仓库名
- 不是 branch 名
- 如果你说的 `niyien` 是一个分支，不需要单独在这里配置

---

## 6. 最短结论

### 最短操作版

首次配置：

1. 123 拿 3 个值
2. GitHub 填 3 个 Secrets
3. Vercel 填 3 个 Env
4. 本地 `control_center.config.json` 填配置

每次发版：

1. 如果要换资源，先去 `资源编排`
2. 设置：
   - `Lens/CameraDB Tag`
   - `Plugin 来源模式`
   - `Plugin Release Tag` 或 `Plugin Artifact 名称`
   - `SDK 基础地址`
3. 点 `保存为下次发版默认源`
4. 打应用 tag
5. 等 Actions 完成
6. 去 `应用发布`
7. 点：
   - `发布新应用，但不推送`
   - 或 `发布并立即推送`

### GitHub Token 最短结论

- 想简单：`classic PAT`，只勾 `repo`
- 想更安全：`fine-grained PAT`
  - `Resource owner`：`NiYien`
  - `Only select repositories`：`gyroflow`
  - 权限只开：
    - `Actions: Read and write`
    - `Contents: Read-only`
