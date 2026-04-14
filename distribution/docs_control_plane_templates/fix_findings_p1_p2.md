# Docs 仓库 Findings 修复方案

目标文件：

- `C:\Users\Jhe\Desktop\github\docs\api\_telemetry-shared.js`
- `C:\Users\Jhe\Desktop\github\docs\api\telemetry-stats.js`

对应 findings：

1. 下载 telemetry 无法生成 rollout health 统计
2. 旧 NiYien Tool 历史数据在未 rebuild 前会从默认 dashboard 消失

## Finding 1 修复：补齐 rollout health 聚合维度

### 目标

让 telemetry keyspace 可以回答这些问题：

- GitHub / 国内源成功率分别是多少
- 哪个平台失败更多
- `lens` 包下载是否异常
- SDK / 插件下载成功率如何

### 要改的核心原则

在 `buildDimensionCountKeys()` / `buildDimensionUniqueKeys()` 中新增这些维度：

- `platform`
- `artifact`
- `selected_source`
- `status`

以及几个组合维度：

- `platform + status`
- `selected_source + status`
- `artifact + status`
- `artifact + selected_source + status`

### 推荐 key 结构

在 `buildDimensionCountKeys(prefix, parts)` 中新增：

```js
if (parts.platform) {
  keys.push(`${prefix}:platform:${parts.platform}`);
}

if (parts.artifactType) {
  keys.push(`${prefix}:artifact:${parts.artifactType}`);
}

if (parts.selectedSource) {
  keys.push(`${prefix}:selected_source:${parts.selectedSource}`);
}

if (parts.status) {
  keys.push(`${prefix}:status:${parts.status}`);
}

if (parts.platform && parts.status) {
  keys.push(`${prefix}:platform:${parts.platform}:status:${parts.status}`);
}

if (parts.selectedSource && parts.status) {
  keys.push(`${prefix}:selected_source:${parts.selectedSource}:status:${parts.status}`);
}

if (parts.artifactType && parts.status) {
  keys.push(`${prefix}:artifact:${parts.artifactType}:status:${parts.status}`);
}

if (parts.artifactType && parts.selectedSource && parts.status) {
  keys.push(
    `${prefix}:artifact:${parts.artifactType}:selected_source:${parts.selectedSource}:status:${parts.status}`
  );
}
```

注意：

- 不要继续用 `:source:` 作为“下载源”标签
- 因为当前 key 前缀里 `source:*` 已经被 `source_app_id` 占用了
- 下载源维度必须用 `selected_source`

### 唯一用户集合 key

在 `buildDimensionUniqueKeys(prefix, parts)` 中新增：

```js
if (parts.platform) {
  keys.push(`${prefix}:unique:platform:${parts.platform}`);
}

if (parts.artifactType) {
  keys.push(`${prefix}:unique:artifact:${parts.artifactType}`);
}

if (parts.selectedSource) {
  keys.push(`${prefix}:unique:selected_source:${parts.selectedSource}`);
}

if (parts.status) {
  keys.push(`${prefix}:unique:status:${parts.status}`);
}
```

## Finding 1 修复：`telemetry-stats.js` 读取这些新维度

### 在 `collectStats()` 新增这些桶

```js
const sourceTotals = {};
const platformTotals = {};
const statusTotals = {};
const artifactTotals = {};
const selectedSourceTotals = {};

const platformStatusTotals = {};
const sourceStatusTotals = {};
const artifactStatusTotals = {};
```

### 在每日扫描中新增这些 patterns

```js
const platformPattern = `${basePrefix}:platform:*`;
const statusPattern = `${basePrefix}:status:*`;
const artifactPattern = `${basePrefix}:artifact:*`;
const selectedSourcePattern = `${basePrefix}:selected_source:*`;
const platformStatusPattern = `${basePrefix}:platform:*:status:*`;
const sourceStatusPattern = `${basePrefix}:selected_source:*:status:*`;
const artifactStatusPattern = `${basePrefix}:artifact:*:status:*`;
```

### 读取并累计

- `platformPattern` -> `platformTotals`
- `statusPattern` -> `statusTotals`
- `artifactPattern` -> `artifactTotals`
- `selectedSourcePattern` -> `selectedSourceTotals`
- `platformStatusPattern` -> `platformStatusTotals`
- `sourceStatusPattern` -> `sourceStatusTotals`
- `artifactStatusPattern` -> `artifactStatusTotals`

### 新增一个双层聚合函数

建议新增：

```js
async function accumulateDoubleTotals(keys, totals, labelA, labelB) {
  if (!keys.length) return;
  const values = await getValues(keys);
  for (let i = 0; i < keys.length; i += 1) {
    const count = parseInt(values[i] || "0", 10);
    if (!count) continue;
    const a = parseKeyValue(keys[i], labelA);
    const b = parseKeyValue(keys[i], labelB);
    if (!a || !b) continue;
    if (!totals[a]) totals[a] = {};
    totals[a][b] = (totals[a][b] || 0) + count;
  }
}
```

### `collectStats()` 返回值新增

建议最终返回：

```js
platform_totals: platformTotals,
status_totals: statusTotals,
artifact_totals: artifactTotals,
selected_source_totals: selectedSourceTotals,
platform_status_totals: platformStatusTotals,
selected_source_status_totals: sourceStatusTotals,
artifact_status_totals: artifactStatusTotals,
```

这样控制中心和 stats 页面就能直接展示：

- GitHub / CN 成功率
- 各平台成功率
- 各资源类型成功率

## Finding 2 修复：为 legacy key 增加回退读取

### 目标

在没有运行 rebuild 之前，旧 `NiYien Tool` 的历史统计也要继续出现在默认视图里。

### 只在这些条件下启用 legacy 回退

建议新增：

```js
function shouldUseLegacyFallback(filters) {
  return (
    filters.productId === DEFAULT_PRODUCT_ID &&
    filters.event === DEFAULT_TELEMETRY_EVENT &&
    (!filters.sourceAppId || filters.sourceAppId === "niyien_tool")
  );
}
```

### legacy count key 读取

在 `collectStats()` 每日循环里，如果 `shouldUseLegacyFallback(filters)` 为真，再额外扫描旧 key：

```js
const legacyCityBrandPattern = `telemetry:day:${day}:city:*:brand:*:event:open`;
const legacyModelPattern = `telemetry:day:${day}:model:*:event:open`;
const legacyLanguagePattern = `telemetry:day:${day}:lang:*:event:open`;
const legacyCountryPattern = `telemetry:day:${day}:country:*:event:open`;
const legacyHourPattern = `telemetry:day:${day}:hour:*:event:open`;
const legacyEventKey = `telemetry:day:${day}:event:open`;
```

把这些结果合并到：

- `cityTotals`
- `brandTotals`
- `modelTotals`
- `languageTotals`
- `countryTotals`
- `hourTotals`

同时把 legacy 总量记到：

```js
sourceTotals["niyien_tool"] += legacyEventCount;
```

### legacy unique key 回退

以下函数都要在 `shouldUseLegacyFallback(filters)` 为真时，把旧 key 一起算进去：

- `getGlobalUniqueTotal()`
- `getScopedUniqueTotals()`
- `collectNewTotals()`
- `collectSourceUniqueTotals()`
- `collectWeeklyUsage()`

#### 旧 unique key

```js
telemetry:day:${day}:unique:all
telemetry:day:${day}:unique:city:${...}
telemetry:day:${day}:unique:brand:${...}
telemetry:day:${day}:unique:model:${...}
telemetry:day:${day}:unique:country:${...}
```

#### 旧 new users key

```js
telemetry:day:${day}:new:all
```

#### 旧 weekly usage key

```js
telemetry:week:${weekKey}:user:*
```

### weekly usage 的兼容处理

对于默认汇总视图：

- 新 key 与旧 key 一起扫描
- 解析 `:user:` 后面的 anon id
- 用 anon id 合并同一用户
- 再计算 buckets

这样能避免未来新旧来源同一用户被硬性重复统计。

### legacy source unique totals

当启用 legacy 回退时：

- `source_unique_totals["niyien_tool"]` 应来自旧 `unique:all` 集合

## 推荐补充返回字段

为了明确当前数据是“新 key”还是“legacy 回退”得到的，建议在 `telemetry-stats` 返回里增加：

```js
legacy_fallback_used: true | false
legacy_days_used: [...]
```

## 改完后的验收

### 1. 下载健康统计

请求：

```text
/api/telemetry-stats?product_id=gyroflow_niyien&event=download_result
```

应返回：

- `platform_totals`
- `status_totals`
- `artifact_totals`
- `selected_source_totals`
- `platform_status_totals`
- `selected_source_status_totals`
- `artifact_status_totals`

### 2. 旧数据兼容

在不跑 rebuild 的情况下，请求：

```text
/api/telemetry-stats?product_id=gyroflow_niyien&event=open
```

应仍能看到旧 `NiYien Tool` 的历史统计。

### 3. 来源拆分

请求：

```text
/api/telemetry-stats?product_id=gyroflow_niyien&source_app_id=niyien_tool&event=open
```

应返回旧 Tool 历史数据。

请求：

```text
/api/telemetry-stats?product_id=gyroflow_niyien&source_app_id=gyroflow_niyien&event=open
```

应只返回新主程序数据。

