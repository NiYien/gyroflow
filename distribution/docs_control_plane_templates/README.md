# Docs 控制面模板说明

当前会话只能写 `gyroflow` 仓库，不能直接写：

- `C:\Users\Jhe\Desktop\github\docs`

所以这里放的是 `docs` 仓库需要落地的模板和改造规则。

## 目标仓库

- `docs` 仓库是唯一控制面
- `gyroflow` 仓库只负责产物

## 需要在 `docs` 仓库新增/修改的文件

### 新增

- `api/manifest.js`
  - 可直接采用当前仓库的 [api/manifest.js](C:/Users/Jhe/Desktop/github/gyroflow/api/manifest.js) 为基础
- `scripts/control_center.py`
  - 可直接采用当前仓库的 [distribution/control_center.py](C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.py)

### 修改

- `api/telemetry.js`
  - 兼容旧 `NiYien Tool`
  - 支持新 `gyroflow(niyien)` 事件
  - 新增 `source_app_id` / `product_id`
  - 新增可选字段：
    - `platform`
    - `artifact_type`
    - `artifact_version`
    - `selected_source`
    - `status`
    - `duration_ms`
    - `bytes`
- `api/telemetry-stats.js`
  - 支持 `product_id`
  - 支持 `source_app_id`
  - 支持 `event`
- `stats.html`
  - 默认产品筛选为 `gyroflow_niyien`
  - 增加来源拆分筛选
- `vercel.json`
  - 增加 `/api/manifest` 的 `no-store` header

## telemetry 兼容规则

### 旧 Tool 不改发送格式也必须继续可用

当请求里没有新字段时，服务端自动补：

```json
{
  "source_app_id": "niyien_tool",
  "product_id": "gyroflow_niyien"
}
```

### 新 Gyroflow 发送

```json
{
  "source_app_id": "gyroflow_niyien",
  "product_id": "gyroflow_niyien"
}
```

## telemetry 推荐事件

推荐至少支持这些事件：

- `open`
- `manifest_fetch`
- `download_result`
- `sdk_download_result`
- `plugin_download_result`
- `manual_version_click`

其中：

- `open` 继续兼容旧统计体系
- 其他事件用于 `gyroflow(niyien)` 分发观测

## telemetry-stats 新查询参数

建议 `docs/api/telemetry-stats.js` 追加支持：

- `product_id`
- `source_app_id`
- `event`

默认行为：

- 如果未传 `product_id`，默认按 `gyroflow_niyien`
- 如果未传 `source_app_id`，看汇总
- 如果未传 `event`，默认看全部或兼容旧 `open`

## manifest 的环境变量来源

`docs` 仓库的 `manifest` 只靠 Vercel 环境变量驱动：

- `NIYIEN_RELEASE_POLICY_JSON`
- `NIYIEN_CONTENT_RELEASE_TAG`
- `NIYIEN_LENS_VERSION`
- `NIYIEN_LENS_SHA256`

不要在 `docs` 仓库里读取 `gyroflow/_deployment/_binaries`。

## 参考文档

- [C:/Users/Jhe/Desktop/github/gyroflow/distribution/DEPLOYMENT.md](C:/Users/Jhe/Desktop/github/gyroflow/distribution/DEPLOYMENT.md)
- [C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.py](C:/Users/Jhe/Desktop/github/gyroflow/distribution/control_center.py)
