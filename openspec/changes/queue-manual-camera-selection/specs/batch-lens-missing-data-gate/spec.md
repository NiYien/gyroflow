## ADDED Requirements

### Requirement: 缺数据门 SHALL 以成功解析的手动相机几何满足 sensor 条件

当全队列手动相机资格成立，且有效品牌/型号已为 job 解析出可用 `unit_pixel_focal_length` 或非裸 camera matrix 时，缺数据门 SHALL 将该结果视为已具备 sensor 数据。若选择缺失、型号不可选、几何不可解析或资格不成立，现有 sensor 缺数据检查 SHALL 继续拦截。

#### Scenario: 有效手动相机解除 sensor 缺失

- **GIVEN** job 的原始元数据缺少传感器数据
- **AND** 手动相机 resolver 已为 job 生成可用几何
- **AND** 对应镜头组焦距有效
- **WHEN** 缺数据门检查该 job
- **THEN** 不报告该 job 缺少 sensor 数据

#### Scenario: 未完成品牌型号选择仍拦截

- **GIVEN** 全队列具备手动相机资格
- **BUT** 品牌或型号仍为空
- **WHEN** 缺数据门检查需要 sensor 数据的 job
- **THEN** 继续报告 sensor 缺失

#### Scenario: 不可选型号仍拦截

- **GIVEN** 保存型号没有任何数值 readout 数据或已从活动数据库移除
- **WHEN** 缺数据门检查需要 sensor 数据的 job
- **THEN** 继续报告 sensor 缺失

#### Scenario: 混合队列不使用保存选择绕过门

- **GIVEN** 保存了有效的手动品牌和型号
- **AND** 队列中存在一个完整识别相机的 video job
- **WHEN** 缺数据门检查其他相机标识不完整的 job
- **THEN** 保存选择不作为有效覆盖
- **AND** 该 job 仍按原始/既有有效数据接受或拦截

### Requirement: readout fallback MUST NOT 绕过焦距或几何缺失

半帧 readout fallback 只 SHALL 补充 rolling-shutter time；它 MUST NOT 被视为焦距、lens_index、传感器宽度或 camera matrix 的替代数据。

#### Scenario: 只有估算 readout 仍缺 sensor

- **GIVEN** 系统可以计算 half-frame readout
- **BUT** 所选型号无法解析传感器几何
- **WHEN** 缺数据门检查该 job
- **THEN** 仍报告 sensor 缺失

#### Scenario: 缺镜头组焦距仍按既有规则拦截

- **GIVEN** 手动相机几何解析成功
- **BUT** job 对应 Lens Group 没有有效焦距且视频也无可用焦距
- **WHEN** 缺数据门检查该 job
- **THEN** 继续报告既有 focal 缺失
