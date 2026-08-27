## ADDED Requirements

### Requirement: 批量镜头应用 SHALL 使用符合资格的手动相机有效元数据

当全队列手动相机资格成立、保存品牌/型号有效、job 已有 lens_index 且对应组焦距有效时，批量镜头应用 SHALL 使用手动相机 resolver 生成的 `unit_pixel_focal_length`、camera matrix 和 readout。该结果 SHALL 基于 job 自身视频参数，且 SHALL 从干净基线应用。

#### Scenario: 缺自动传感器数据时由手动相机补足

- **GIVEN** job 原始元数据没有可用 `unit_pixel_focal_length`
- **AND** 全队列具备手动相机资格且保存选择可解析
- **AND** job 已匹配一个焦距有效的镜头组
- **WHEN** 批量镜头应用该配置
- **THEN** profile 使用手动相机解析出的 `unit_pixel_focal_length`
- **AND** `fx` 等于组焦距乘以解析出的 `unit_pixel_focal_length`

#### Scenario: 每个 job 使用自身视频参数

- **GIVEN** 两个 job 使用同一手动相机和同一镜头组
- **AND** 两个 job 的分辨率、帧率或模式提示不同
- **WHEN** 批量镜头应用配置
- **THEN** 两个 job 分别解析自己的 camera matrix 和 readout

#### Scenario: 无镜头号时维持既有行为

- **GIVEN** job 没有显式或匹配得到的 lens_index
- **WHEN** 全队列手动相机资格成立
- **THEN** 批量应用不把该 job 默认为 L1
- **AND** 现有匹配、跳过或缺数据行为保持不变

#### Scenario: 失去资格后从基线重建

- **GIVEN** job 先前已应用手动相机生成的 matrix 和 readout
- **WHEN** 队列因任一视频完整识别相机而失去资格
- **THEN** 批量镜头应用从该 job 的干净基线重建
- **AND** 不保留此前手动相机生成的值

### Requirement: 队列 job 工程状态 SHALL 保存手动相机解析结果

成功解析的 camera matrix 和 readout SHALL 写入 job 的工程状态，使渲染、导出、重新导出和队列 Play 消费同一结果，而不修改原始检测元数据。

#### Scenario: 渲染与队列 Play 一致

- **GIVEN** job 已通过手动相机解析应用镜头 profile
- **WHEN** 该 job 被渲染并随后从队列 Play 打开
- **THEN** 两条路径使用相同的 camera matrix、焦距和 readout

#### Scenario: 重新应用不会累积旧覆盖

- **GIVEN** 用户先后选择两个不同的可用相机型号
- **WHEN** 队列完成第二次重算
- **THEN** job 状态仅反映第二个型号从干净基线生成的结果

