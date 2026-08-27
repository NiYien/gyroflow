## Context

Lens Group 当前只保存 L1-L6 各自的焦距/变宽参数。队列 job 若没有完整的相机品牌和型号，镜头 profile 构建就可能缺少 `unit_pixel_focal_length`，因此即使用户填写了焦距，也不能得到正确的 `fx/fy`；同一缺口也使 rolling-shutter readout 无法从 camera_db 解析。

本变更面向一类明确的批处理场景：队列非空，且每个 video job 的原始 `camera_identifier.brand`、`camera_identifier.model` 至少缺一项。此时 Lens Group 面板提供一份全局手动相机选择，供所有已分配 L1-L6 的 job 使用。只要任一 video job 拥有完整品牌+型号，手动选择即整体失效并从 UI 隐藏。

现有 camera_db 已实现按品牌组织的机型、传感器宽度、裁切规则和 readout 档位/回退。实现必须使用当前活动 camera_db 路径（包括运行时 lens-data 包），而不是复制一份静态目录或改变自动遥测 parser。

## Goals / Non-Goals

**Goals:**

- 仅在整批视频全部无法完整识别相机时，自动显示并启用全局 Camera brand / Camera model 选择。
- 长期保存所选品牌和型号，但让保存值与“当前是否具备资格”相互独立。
- 为每个 job 使用其自身分辨率、帧率、已有模式/裁切提示和对应 Lens Group 焦距解析有效相机几何与 readout。
- 复用 camera_db 的机型、crop、readout 规则，并保留 camera_db 热更新能力。
- 不污染原始检测元数据；队列重新评估资格时仍以原始检测结果为准。
- 保持现有缺数据门、队列 Play、重新匹配和导出路径一致。

**Non-Goals:**

- 不为每个 L1-L6 增加独立相机选择。
- 不增加用户可见的启用开关、应用数量、解析来源、fallback 提示或 readout 输入框。
- 不改变现有 `lens_group_manual_edit` 的含义。
- 不让手动相机选择影响直接打开的主预览原始视频。
- 不修改 camera_db 数据文件或相机品牌 parser 的自动检测结果。
- 不把缺少 lens_index 的 job 默认分配给 L1。

## Decisions

### 1. 使用“原始资格 + 有效覆盖”双层模型

队列维护两类信息：

- 原始检测元数据：用于判断整个队列是否具备手动相机资格，永不被本功能写回。
- 有效 job 元数据：仅在资格成立且保存的品牌/型号可解析时，叠加手动相机解析结果，供镜头 profile、缺数据门和 job 工程序列化使用。

资格定义为：队列至少包含一个 video job，且所有 video job 都不存在非空的完整品牌+型号。非视频条目不参与判定。任一 video job 完整识别时，不论保存值是什么，手动覆盖对整个队列均不生效。

这样避免“第一次覆盖后 job 看起来已经有相机，下一次刷新反而关闭自己”的反馈环，也保证移除/新增 job 后能从原始事实重新计算。

### 2. 一份全局持久化选择，不改变六槽 Lens Group schema

新增独立设置键保存手动 camera brand/model。L1-L6 继续只保存各自镜头配置；解析每个 job 时，将全局相机选择与该 job 已有的 lens_index 对应焦距组合。

保存值即使暂时不具备资格、品牌消失或型号被禁用也保留。恢复资格且目录仍有效时自动重新使用；不通过 UI 自动清空用户选择。

### 3. 相机目录来自当前活动 camera_db

新增集中、可单测的 camera catalog/resolver 层：

- 通过 `gyro_source::get_camera_db_path()` 获取当前活动目录。
- 以目录中的品牌数据库为入口，使用 `CameraDatabase` 读取 `BrandData.models`、传感器宽度、crop 和 readout 数据。
- 型号只要任意 readout 档位存在数值（包括 `0`，代表 global shutter）即可选择；完全没有数值 readout 的型号仍展示但禁用。
- lens-data 包更新后重新加载目录、校验保存值并通知队列重算。

目录枚举和解析集中在 core helper 中，QML 不直接读取 JSON，也不自己实现 camera_db 规则。

### 4. 逐 job 解析几何，不生成共享 camera matrix

解析输入包括：手动品牌/型号、job 宽高和帧率、原有模式/裁切提示，以及其 lens_index 对应的焦距。resolver 使用 camera_db 的传感器宽度和 crop 匹配得到有效画幅，再计算：

`unit_pixel_focal_length = effective_image_width / effective_sensor_width`

`fx = focal_length_mm * unit_pixel_focal_length`

`fy` 继续沿用现有镜头 profile 的像素宽高比/变宽处理链。

品牌已有特殊画幅读取语义时，由集中 resolver 对齐现有品牌 parser 的规则；普通机型走 camera_db model/crop 数据。解析失败不猜测传感器宽度，继续交给缺数据门。

### 5. readout 先用 camera_db，再使用不可见半帧回退

对每个 job 调用 camera_db 的 readout lookup，传入该 job 的分辨率、帧率和可用模式提示，保留其既有精确档位、分辨率 class 和帧率 fallback 行为。返回 `0` 是有效结果，不视为缺失。

若 camera_db 对该具体 job 仍无结果，则从 `{25, 30, 50, 60, 100, 120, 200, 240}` 中选取与 job 帧率绝对差最小的标准帧率，计算 `500 / nearest_fps` 毫秒。相同距离时选择较小帧率以保持确定性。

已有 readout direction 保留；没有方向时使用 `TopToBottom`。该 fallback 不在 UI 展示，也不增加额外设置。

### 6. 队列生命周期统一触发重算

下列事件重新计算资格，并在必要时从干净的 job 基线重新应用镜头 profile：

- 添加、移除、清空、替换或载入队列 job；
- 保存的品牌或型号变化；
- Lens Group 配置变化或重新匹配；
- 活动 camera_db/lens-data 包更新。

`match_results` 改变 job 是否属于 CalibrationPair 时也属于队列资格变化：该 job 进入或离开 video job 集合后，系统重新发出 UI 状态并按新集合清理或恢复覆盖。

镜头/相机重算在后台执行，因此每次手动相机上下文变化都递增 generation；每个 job 以独立 guard 串行修改自己的 stabilizer。worker 开始、取得 guard 后和主线程回写工程状态前都校验 generation，过期结果不得覆盖更新的品牌/型号或资格状态。match apply 若跨越 generation 变化，保留其匹配结果，但丢弃过期的相机工程快照并用最新上下文再重建一次。资格翻转需要 post-match 全量重建时，`match_apply_finished` 由该 full-reapply epoch 构成完成屏障：只有最新目标回调提交后才发信号，避免 QML 提前继续同步或导出。

已完成渲染的 job 可能为节省内存释放 stabilizer，因此 job 同时记录其 `project_data` 所代表的手动相机 generation。若重置/重新渲染时快照已过期，系统先恢复 stabilizer 并按当前 generation 从干净基线重建；重建回调提交新的工程状态之前，该 job 不得重新进入 Queued。重复 reset 在 pending 状态下幂等返回；同步调用方提出的 queue start、batch autosync 或 direct render 会保存为 continuation，并在最后一个 pending rebuild 提交后恢复。这样即使资格在 job 完成后才失效，旧手动相机快照也不能绕过队列级条件或吞掉用户刚发起的操作。

从队列 Play 打开 job 时继续加载该 job 已写入的工程状态，因此主预览能看到与渲染相同的 matrix/readout；直接打开原始视频不注入手动选择。

### 7. UI 只有两个级联选择器

Lens Group 面板仅在资格成立时增加 `Camera brand` 和 `Camera model` 两个下拉框。选择品牌后刷新型号；不可用型号保留在列表中但不可选。无可用型号或未完成选择时不增加隐式默认值。

不显示额外开关、应用范围、视频计数、读取时间、fallback 或错误状态。错误通过既有缺数据门和内部日志表达。

## Risks / Trade-offs

- **活动数据库发生变化：** 保存的品牌/型号可能不再存在。实现保留保存值但将其视为不可解析，不向 job 写入部分结果，并让现有 gate 拦截。
- **品牌特殊画幅逻辑容易漂移：** resolver 必须集中测试已知特殊品牌/机型，并尽量调用 camera_db/现有 helper，而不是在 QML 或队列代码中散落公式。
- **队列重算成本：** catalog 只在路径/数据更新时加载；普通资格或选择变化复用内存数据库，并只重建受影响的 job 状态。
- **混合可识别队列牺牲手动覆盖：** 这是有意的全局安全规则；它避免同一批次混用自动与人工机型语义。
- **半帧 readout 是估算：** 仅在可选择型号对具体 job 没有匹配档位时使用，且不伪装成 camera_db 原始值；内部结果应可被测试和日志区分。

## Migration Plan

1. 增加独立的手动品牌/型号设置键；旧设置不存在时默认空值。
2. 增加 catalog/resolver 及单元测试，不改变既有自动检测路径。
3. 把有效覆盖接入队列镜头应用、缺数据检查和 job 工程状态。
4. 增加严格条件 UI，并接入设置和重算信号。
5. 对空队列、全缺失、混合队列、数据库热更新、无 readout 型号及具体档位 fallback 做回归测试。

回滚时可移除 UI 和有效覆盖调用；独立设置键即使残留也不会影响旧版本，现有 `lens_group_configs_v1` 无需迁移。

## Open Questions

无。产品语义、适用范围、持久化、可选型号规则和 readout fallback 均已确认。
