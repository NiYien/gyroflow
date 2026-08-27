## ADDED Requirements

### Requirement: 系统 SHALL 仅在整批视频均无完整相机标识时启用手动相机资格

系统 SHALL 以每个 video job 的原始检测元数据判定资格：队列非空，且所有 video job 的相机品牌与型号不能同时为非空。非视频 job SHALL 不参与“全部视频”的判定。有效覆盖后的元数据 MUST NOT 反向影响资格。

#### Scenario: 空队列不具备资格

- **WHEN** 渲染队列不包含 video job
- **THEN** 手动相机资格为 false

#### Scenario: 所有视频都缺少完整标识

- **GIVEN** 队列包含一个或多个 video job
- **AND** 每个 video job 的品牌或型号至少缺一项
- **WHEN** 系统计算手动相机资格
- **THEN** 资格为 true

#### Scenario: 任一视频完整识别则整批失效

- **GIVEN** 至少一个 video job 的原始品牌和型号均非空
- **WHEN** 系统计算手动相机资格
- **THEN** 资格为 false
- **AND** 手动相机覆盖不应用到任何 job

#### Scenario: 有效覆盖不造成自我关闭

- **GIVEN** 全部 video job 原始标识不完整且手动相机覆盖已写入有效状态
- **WHEN** 队列重新计算资格
- **THEN** 系统仍仅查看原始检测元数据
- **AND** 资格保持 true

### Requirement: 系统 SHALL 长期保存一份队列全局相机选择

系统 SHALL 使用独立设置保存一个 camera brand 和一个 camera model。保存值 SHALL 独立于 L1-L6 配置和 `lens_group_manual_edit`，并在资格不成立时保留但不生效。

#### Scenario: 重启后恢复选择

- **WHEN** 用户选择品牌和型号并重启应用
- **THEN** 系统恢复相同的保存值

#### Scenario: 混合队列暂时禁用但不清空

- **GIVEN** 已保存有效的品牌和型号
- **WHEN** 一个可完整识别相机的 video job 进入队列
- **THEN** 手动覆盖立即失效
- **AND** 保存的品牌和型号保持不变

#### Scenario: 恢复全缺失队列后重新生效

- **GIVEN** 已保存的品牌和型号仍存在且可选
- **WHEN** 队列恢复为所有 video job 均无完整标识
- **THEN** 系统无需再次选择即可使用保存值

### Requirement: 相机目录 SHALL 使用当前活动 camera_db

系统 SHALL 从 `get_camera_db_path()` 指向的活动 camera_db 枚举品牌和型号，并通过 `CameraDatabase` 读取 model、crop 与 readout 数据。型号若完全没有数值 readout 数据 SHALL 保留在目录中但标记为不可选；数值 `0` SHALL 视为有效 readout 数据。

#### Scenario: 运行时数据库提供目录

- **WHEN** 活动 lens-data 包改变 camera_db 路径或内容
- **THEN** 相机目录从新的活动路径重新加载
- **AND** 队列重新校验保存选择并重算有效状态

#### Scenario: 无 readout 数值的型号禁用

- **GIVEN** 某型号的 readout 表不包含任何数值
- **WHEN** UI 获取相机目录
- **THEN** 型号仍在对应品牌列表中
- **AND** 型号不可选择

#### Scenario: global shutter 型号可选

- **GIVEN** 某型号至少一个 readout 数值为 `0`
- **WHEN** UI 获取相机目录
- **THEN** 型号可选择

### Requirement: 系统 SHALL 为每个 job 解析独立的有效相机几何

当资格成立且保存选择有效时，系统 SHALL 使用全局品牌/型号、job 自身的分辨率/帧率/已有模式提示以及其 lens_index 对应焦距，按 camera_db 和品牌规则解析 crop、`unit_pixel_focal_length`、`fx/fy`。没有 lens_index 的 job MUST NOT 默认使用 L1。

#### Scenario: 同一相机在不同分辨率分别计算

- **GIVEN** 两个符合资格的 job 使用相同手动相机但分辨率不同
- **AND** 两个 job 均已分配 lens_index 且对应焦距有效
- **WHEN** 系统应用手动相机覆盖
- **THEN** 每个 job 使用自身分辨率和 crop 规则计算相机矩阵

#### Scenario: 六个镜头号共享相机但使用各自焦距

- **GIVEN** 多个 job 分别分配到 L1-L6 中的不同组
- **WHEN** 系统应用手动相机覆盖
- **THEN** 所有 job 使用同一手动品牌和型号
- **AND** 每个 job 使用其对应镜头组焦距

#### Scenario: 未分配镜头号不猜测 L1

- **GIVEN** 一个 job 没有 lens_index
- **WHEN** 系统应用手动相机覆盖
- **THEN** 系统不为其选择 L1 焦距
- **AND** 继续沿用现有匹配或缺数据行为

#### Scenario: 解析失败不写入部分几何

- **GIVEN** 保存的型号不存在、不可选或缺少可解析传感器几何
- **WHEN** 系统尝试创建有效覆盖
- **THEN** 系统不伪造 `unit_pixel_focal_length` 或 camera matrix
- **AND** 原始元数据保持不变

### Requirement: 系统 SHALL 按 camera_db 优先级解析逐 job 帧读出时间

系统 SHALL 首先使用 camera_db 对所选型号、job 分辨率、帧率和可用模式提示执行既有 readout lookup。若没有结果，系统 SHALL 选择 `{25, 30, 50, 60, 100, 120, 200, 240}` 中距离 job 帧率最近的值，并使用 `500 / nearest_fps` 毫秒。相同距离 SHALL 选择较小帧率。

#### Scenario: 数据库匹配优先于估算

- **GIVEN** camera_db 能为 job 找到 readout
- **WHEN** 系统解析有效相机数据
- **THEN** 使用 camera_db 返回值
- **AND** 不使用半帧估算

#### Scenario: 具体档位缺失时使用半帧估算

- **GIVEN** 型号可选但 camera_db 对当前 job 没有 readout 结果
- **AND** job 帧率最接近 60 fps
- **WHEN** 系统解析有效相机数据
- **THEN** readout time 为 `500 / 60` 毫秒

#### Scenario: 零 readout 保持为有效数据库结果

- **GIVEN** camera_db 为 job 返回 `0`
- **WHEN** 系统解析有效相机数据
- **THEN** readout time 保持为 `0`
- **AND** 不使用半帧估算

#### Scenario: 方向缺失时使用默认方向

- **GIVEN** job 没有已有 readout direction
- **WHEN** 手动相机解析写入 readout time
- **THEN** direction 为 `TopToBottom`

### Requirement: 手动相机覆盖 SHALL 仅影响队列 job 的有效状态

系统 MUST NOT 修改 `FileMetadata.camera_identifier` 等原始检测结果。覆盖结果 SHALL 写入队列 job 的工程状态，供渲染、导出与队列 Play 使用；直接打开原始视频 SHALL 不继承该覆盖。

#### Scenario: 原始标识保持不变

- **WHEN** 系统为 job 应用手动相机覆盖
- **THEN** 原始检测品牌和型号保持原值

#### Scenario: 队列 Play 使用已解析状态

- **GIVEN** job 已成功应用手动相机矩阵和 readout
- **WHEN** 用户从队列点击 Play
- **THEN** 主预览载入该 job 的同一工程状态

#### Scenario: 直接打开原始文件不受影响

- **GIVEN** 相同原始视频存在于已覆盖的队列 job 中
- **WHEN** 用户直接打开原始视频文件而不是从队列 Play
- **THEN** 主预览不注入队列手动相机选择

### Requirement: 影响相机解析的事件 SHALL 重新应用队列状态

品牌/型号变化、资格变化、Lens Group 配置变化以及活动 camera_db 更新时，系统 SHALL 从干净 job 基线重新计算受影响 job，避免旧 matrix 或 readout 残留。

#### Scenario: 选择型号后立即重算

- **WHEN** 用户在具备资格的队列中选择另一个可用型号
- **THEN** 所有符合应用条件的 video job 立即重新解析并应用

#### Scenario: 失去资格时移除旧覆盖

- **GIVEN** 手动覆盖已生效
- **WHEN** 一个完整识别相机的 video job 使队列失去资格
- **THEN** 系统从干净基线重建此前受手动覆盖影响的 job
- **AND** 旧的手动 matrix/readout 不残留

#### Scenario: 快速切换只提交最新选择

- **GIVEN** 品牌选择触发的后台重算尚未结束
- **WHEN** 用户又选择型号并触发新的重算
- **THEN** 旧 generation 不得回写 metadata、LensProfile 或 job 工程状态
- **AND** 最终状态只反映最新的完整品牌与型号

#### Scenario: CalibrationPair 变化重新计算资格

- **GIVEN** `match_results` 使一个 job 进入或离开 CalibrationPair 集合
- **WHEN** 系统重新确定参与判定的 video job 集合
- **THEN** 手动相机 UI 状态和有效覆盖按新集合刷新
- **AND** 若资格发生变化，系统从干净基线全量重建受影响 job
- **AND** `match_apply_finished` 仅在该重建提交后发出

#### Scenario: 已释放 job 重新排队前清理过期快照

- **GIVEN** 已完成 job 释放了 stabilizer 且其工程快照包含旧 generation 的手动相机覆盖
- **WHEN** 用户重置或重新渲染该 job
- **THEN** 系统先按当前资格和选择从干净基线重建工程状态
- **AND** 重建提交前该 job 不得进入 Queued
- **AND** 用户已发起的 queue start、batch autosync 或 direct render 在提交后继续执行
