# 遥测 Upstash 命令预算与客户端 24 小时限流 - 设计文档

日期: 2026-07-25

状态: 已与用户逐节确认，待用户复核

## 1. 背景与根因

生产遥测入口位于 `C:\Users\Jhe\Desktop\github\docs\api\telemetry.js`。当前每个事件会实时展开为两套聚合维度（产品总计和来源总计），并对每个计数、唯一用户集合、周使用量、新用户上下文和 raw stream 分别执行 Redis 命令。

本地使用生产代码构造代表性 payload 后得到:

| 事件 | 单事件 Upstash Commands |
|---|---:|
| `open` | 79-87 |
| `manifest_fetch` | 119-127 |
| `download_result` | 119-127 |

REST pipeline 只合并 HTTP 请求，不减少 Upstash 对内部 Redis 命令的计费。v1.6.3 每次启动都会检查 manifest，并在成功或失败后上报 `manifest_fetch`。因此 500K/月只支持约 135 次启动/天；若同一会话再上报一个 `open`，只支持约 78-84 次会话/天。

反馈提交和遥测共用同一 Upstash 数据库。反馈不是主要消耗源，但额度耗尽会连带导致反馈确认写入失败。

## 2. 已确认决策

用户已逐项确认以下方向:

1. 所有客户端遥测事件共用一个全局 24 小时窗口。
2. 24 小时内最多尝试上传一次，而不是每种事件一次。
3. 请求一旦获准，无论成功、断网或服务端错误，都立即进入 24 小时冷却，不重试。
4. 服务端入口只写 raw stream，不做实时多维聚合。
5. 统计改为每天基于 raw stream 重建一次，允许最多约 24 小时延迟。
6. 删除当天部分重建，只保留对昨天完整 UTC 日的重建。

## 3. 目标

- G1: 新客户端每台安装在任意连续 24 小时内最多尝试一次遥测请求。
- G2: 客户端重启、同进程并发和系统时间回拨都不能绕过限流。
- G3: 服务端每个接收事件只产生一条 Upstash 写命令。
- G4: 旧客户端不更新也能立即受益，其每个事件从 79-127 条命令降为一条。
- G5: 保留现有原始事件字段和主要统计口径，包括事件计数、唯一用户、产品活跃、新用户、迁移用户和周使用频率。
- G6: raw 数据成为可恢复的权威源；重建失败不能删除 raw。
- G7: 不访问生产 Redis，不自动迁移或删除既有数据。

## 4. 非目标

- N1: 不保证当天实时统计；当前 UTC 日在次日重建前可以为空或不完整。
- N2: 不为遥测请求增加失败重试、离线队列或补发机制。
- N3: 不保留一天内相机、下载和插件事件的完整覆盖。首个获准事件通常是 `manifest_fetch`，其余事件会被客户端限流丢弃。
- N4: 不继续维护事件级 Redis 去重。重复 raw 事件可能使事件次数偏高；唯一用户集合仍天然去重。
- N5: 不在本次改动中重构统计页面 UI 或增加新的遥测设置。
- N6: 不修改反馈记录的数据结构和业务流程。

## 5. 客户端 24 小时限流

### 5.1 统一入口

限流放在 `src/distribution.rs::report_event_internal`，覆盖:

- `manifest_fetch`；
- `open`；
- `download_result`；
- `sdk_download_result`；
- `plugin_download_result`；
- 后续复用该入口的其他遥测事件。

调用方不各自实现限流，避免事件新增后漏接。

### 5.2 持久化状态

应用设置键固定为 `telemetryLastAttemptAtMs`，值为上次获准遥测尝试的 Unix 毫秒时间。窗口常量固定为 `TELEMETRY_ATTEMPT_INTERVAL_MS = 86_400_000`。

判断规则:

```text
last_attempt == 0                 -> allow
now >= last_attempt + 24h         -> allow
now < last_attempt                -> deny
otherwise                         -> deny
```

系统时间回拨时拒绝，不把未来时间戳清零。只有当前时间重新超过记录值 24 小时后才允许。

### 5.3 并发和写入时机

进程内使用单一互斥门完成“读取 -> 判断 -> 写入”，不能先解锁再启动请求。获准时按以下顺序执行:

```text
acquire gate
  -> read persisted timestamp
  -> decide
  -> write current timestamp to settings map
  -> flush settings file with a checked result
release gate
  -> build payload
  -> spawn network request
```

现有 `settings::flush()` 不返回磁盘写入结果，只记录错误。新增 `settings::flush_checked() -> std::io::Result<()>`，现有 `flush()` 保持签名和行为兼容。只有时间戳成功写入 settings 文件后才启动网络线程；持久化失败时跳过发送并保留进程内时间戳，防止同一进程持续重试。网络失败不回滚时间戳，严格保证最多一次尝试。

### 5.4 可观测性

首次放行和因 24 小时窗口跳过使用 `target="update"` 的 Debug 日志。日志不包含 anon id、IP 或完整 payload，只记录事件名、决策和剩余窗口摘要。放行日志每次获准时记录；跳过日志由进程级 `AtomicBool` 限制为本进程第一次跳过时记录。

## 6. 服务端 raw-only 写入

### 6.1 入口行为

`docs/api/telemetry.js` 保留:

- POST 和 JSON 校验；
- batch payload 兼容；
- 字段规范化和 app identity 推断；
- Vercel 地理信息；
- raw event 字段构造；
- 现有成功和错误响应的兼容字段。

每个有效事件只执行:

```text
XADD telemetry:raw:<UTC-day> * <raw fields>
```

删除入口中的事件去重 `SET NX`、实时计数、唯一集合、first-seen、新用户、迁移用户、周计数和逐事件 raw `EXPIRE`。

batch 请求仍逐事件处理，因此 N 个有效事件严格对应 N 条 `XADD`。首版不把多个事件编码进一个 stream entry，保持重建输入和现有 raw schema 兼容。

### 6.2 raw 保留期

raw TTL 沿用 `TELEMETRY_RAW_TTL_DAYS`，默认 365 天。TTL 由当天成功完成的重建任务对 stream key 设置一次。

如果重建失败，不执行 raw `EXPIRE`。这会让数据保留更久，而不是提前丢失；修复后可通过手动重建恢复。

### 6.3 去重取舍

当前 Gyroflow payload 不发送稳定的 `event_id` 或 `ts`，服务端生成值包含接收时间，因此现有 Redis 去重对跨秒重试基本无效。客户端遥测请求也没有重试循环。删除事件去重可将入口从至少两条命令降为一条。

显式携带稳定 `event_id` 的旧客户端若重复请求，raw 中可能出现重复项。这是已接受取舍。重建时用户集合仍按 anon id 去重，只有事件次数可能重复。

## 7. 每日完整重建

### 7.1 调度

`docs/vercel.json` 删除:

```text
30 2 * * * -> scope=today
```

保留:

```text
15 0 * * * -> scope=yesterday
```

自动 cron 使用 `resetDayKeys=false`，因为昨天的日聚合在 raw-only 模式下尚未写入，不再为每个新日期执行 `SCAN + DEL`。普通计数通过 `SET` 覆盖，集合写入具有成员幂等性。手动 `telemetry-rebuild` 接口继续支持指定日期、日期范围和显式 reset，用于修复历史错误或移除 stale keys。

每个 UTC 日正常情况下只重建一次。

### 7.2 日聚合

重建读取 `telemetry:raw:<day>` 全部 entry，复用 `extractEventFields` 和 `buildEventAggregationPlan`，构造:

- `countKeys` 的精确计数；
- `uniqueKeys` 的 anon id 集合；
- `productUniqueKeys` 的 anon id 集合；
- `migratedUserKeys` 的 anon id 集合；
- event-level 和 product-level first-seen/new-user 候选；
- `weekUserKeys` 的用户事件次数。

普通计数使用单条 `SET key value EX ttl`。集合按 key 合并成员后使用一条多成员 `SADD`，再对该聚合 key 设置一次 TTL。所有命令继续按安全大小分块 pipeline，但命令预算按 pipeline 内命令数核算。

### 7.3 first-seen 与新用户

现有重建只处理 event-level `dayNewUserContexts`，不足以替代实时入口。新实现必须同时处理:

- 产品全来源 first-seen；
- 产品指定来源 first-seen；
- event 全来源 first-seen；
- event 指定来源 first-seen；
- 从 NiYien Tool 继承 identity 的 migrated 集合；
- 乱序事件把 first-seen 前移并从旧日期集合迁移。

读取使用去重后的批量 `MGET`。产品 first-seen 缺失时，继续从已知 event first-seen keys 中选择最早日期，避免部署后把存量用户误判为新用户。

手动日期范围重建必须按日期升序处理，保证跨日 first-seen 状态确定。

### 7.4 周使用频率

`weekUserKeys` 是 ISO 周累计，不能用单日计数直接覆盖。每天重建昨天时，需要读取昨天所在 ISO 周截至昨天的所有 raw day streams，重新构造该周的用户次数，然后完整替换对应周聚合结果。

周重建先扫描并删除精确 pattern `telemetry:week:<ISO-week>:*`，再用该周截至目标日的全部 raw events 重写所有产品、事件和来源的周 keys。该 pattern 不能触及其他 ISO 周。手动重建跨周时，每个受影响 ISO 周只重算一次。

### 7.5 重建失败

重建摘要必须记录:

- raw events 数；
- XRANGE 页数；
- 各类聚合 key 数；
- Redis 命令数；
- new-user 创建、修正数；
- raw TTL 是否设置；
- 失败阶段。

任何阶段失败均返回非 2xx，使 Vercel Cron 标记失败。raw stream 不删除。手动重建保持恢复路径。

## 8. 统计语义

现有 `telemetry-stats.js` 继续读取聚合 keys，不在请求时扫描 raw。历史聚合和新重建聚合使用同一 key schema，因此统计页无需同时维护两套读取逻辑。

产品级活跃用户仍跨事件去重。事件级表格只反映实际上传的首个事件；新客户端通常只留下 `manifest_fetch`，所以 `open` 的相机分布会逐步失去代表性。这是全局一次/24 小时决策的直接结果，不通过服务端猜测或复制事件来伪造。

统计页本身没有自动轮询，但一次查询可能消耗大量 `SCAN/MGET/SUNIONSTORE`。本次不改查询结构；保留 token 鉴权，并把该消耗作为后续独立优化项。

## 9. 上线和兼容窗口

上线顺序固定为:

1. 在生产 `docs` 仓库部署 raw-only 入口、完整重建和单次 cron。
2. 观察至少一次 yesterday rebuild 的摘要和 contract tests 结果。
3. 发布带 24 小时限流的 Gyroflow 客户端。

服务端先上线可立即保护额度，包括仍在使用旧客户端的用户。若在 UTC 日中途切换，当天切换前已有实时聚合，切换后只有 raw；次日重建以全天 raw 为准覆盖计数并补齐集合。

不删除历史 raw、日聚合、first-seen 或反馈 keys。回退服务端入口时，既有 raw schema 仍兼容旧实时写入实现。

## 10. 测试策略

### 10.1 Rust 客户端

先写失败测试，再实施:

- 从未记录时允许；
- `24h - 1ms` 拒绝；
- 恰好 24 小时允许；
- 上次时间在未来时拒绝；
- 使用测试 settings 文件模拟重启后仍拒绝；
- 多线程同时竞争只有一个调用获准；
- settings 持久化失败时不启动网络请求；
- 失败不清除已写入的时间戳。

时间判断提取为不访问网络的纯函数。持久化和互斥门使用真实 settings 测试，不通过伪造网络响应证明限流。

### 10.2 Node 服务端

新增或改造 contract tests:

- 单事件入口只向 fake Upstash 发送一条 `XADD`；
- batch 中 N 个事件产生 N 条 `XADD`；
- raw 字段与当前 schema 一致；
- 代表性 `open`、`manifest_fetch`、`download_result` 重建后生成正确 count/unique keys；
- 产品活跃跨事件去重；
- 返回用户不计为新用户；
- generated/adopted identity 的迁移口径正确；
- 乱序事件把 first-seen 前移；
- 完整 ISO 周重建不丢失前几天计数；
- 重建成功才设置 raw TTL；
- `vercel.json` 只保留 yesterday cron。

现有 `_telemetry-newusers.contract.test.mjs` 从“入口后立即验证聚合”调整为“入口写 raw -> 执行重建 -> 验证聚合”，继续测试真实生产模块，只替换 Redis 和 geo 边界。

## 11. 命令预算验收

必须由 fake Upstash 记录实际命令数组，不只依赖人工公式。

验收上限:

| 路径 | 改动前 | 改动后目标 |
|---|---:|---:|
| `open` ingestion | 79-87 | 1 |
| `manifest_fetch` ingestion | 119-127 | 1 |
| `download_result` ingestion | 119-127 | 1 |
| 新客户端安装/24h | 最多多个事件 | 最多 1 个事件、1 条 ingestion command |
| 自动日重建 | 每日 today + yesterday | 每日 yesterday 一次 |

日重建命令数随活跃维度和用户数变化，不设虚假的固定上限，但响应摘要和测试必须报告命令分类，便于后续继续压缩。

## 12. 影响模块

Gyroflow 客户端仓库:

- `src/distribution.rs`：持久化 24 小时限流、并发门、日志和单测。
- `src/core/settings.rs`：增加不破坏现有调用方的 `flush_checked()` API。

niyien.com 生产 `docs` 仓库:

- `api/telemetry.js`：raw-only ingestion；
- `api/telemetry-rebuild.js`：完整日/周聚合、TTL 和摘要；
- `api/telemetry-rebuild-cron.js`：保持 yesterday 调用和失败语义；
- `api/_telemetry-shared.js`：共享聚合计划和必要 helper；
- `api/_telemetry-newusers.contract.test.mjs` 及新增 contract tests；
- `vercel.json`：删除 today cron。

## 13. 风险与约束

- R1: 客户端通常先发送 `manifest_fetch`，相机和下载事件会被压掉。该数据损失已由用户明确选择。
- R2: 统计最多延迟约 24 小时，且上线当天存在短暂混合口径。
- R3: 删除 Redis 去重后，显式重发可能增加事件次数；唯一用户不受影响。
- R4: 周重建若只处理一天会覆盖错误，因此必须按完整 ISO 周重算。
- R5: `docs` 是实际 Vercel 生产仓库；gyroflow 下同名 `api/` 文件只是模板，不能误改模板代替生产代码。
- R6: `docs` 位于当前 gyroflow 仓库之外。实施前必须再次获得用户对跨仓库写入的明确许可。
