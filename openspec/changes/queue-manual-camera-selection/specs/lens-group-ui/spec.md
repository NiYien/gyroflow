## ADDED Requirements

### Requirement: Lens Group 面板 SHALL 按全队列资格显示手动相机选择

Lens Group 面板 SHALL 仅在渲染队列至少包含一个 video job，且所有 video job 的原始相机品牌+型号均不完整时，显示一份队列全局 Camera brand 和 Camera model 选择。该显隐逻辑 SHALL 自动执行，不提供用户可见开关，且 SHALL 与现有 `lens_group_manual_edit` 开关相互独立。

#### Scenario: 全部视频无法完整识别时显示

- **GIVEN** 队列非空
- **AND** 每个 video job 的原始品牌或型号至少缺一项
- **WHEN** 用户打开 Lens Group 面板
- **THEN** 面板显示 Camera brand 和 Camera model 选择

#### Scenario: 任一视频完整识别时隐藏

- **GIVEN** 队列中至少一个 video job 的原始品牌和型号均非空
- **WHEN** Lens Group 面板刷新
- **THEN** Camera brand 和 Camera model 选择均隐藏

#### Scenario: 空队列隐藏

- **GIVEN** 队列不包含 video job
- **WHEN** 用户打开 Lens Group 面板
- **THEN** Camera brand 和 Camera model 选择均隐藏

#### Scenario: Manual edit 不控制手动相机显隐

- **GIVEN** 队列具备手动相机资格
- **WHEN** 用户打开或关闭现有 Manual edit 开关
- **THEN** Camera brand 和 Camera model 选择仍保持显示

### Requirement: 手动相机 UI SHALL 只提供两级选择

手动相机区域 SHALL 只提供 Camera brand 和 Camera model 两个级联选择器。选择品牌 SHALL 更新型号列表；完全没有数值 readout 数据的型号 SHALL 保留显示但禁用。界面 MUST NOT 增加显式启用开关、应用到视频数量、解析来源、half-frame 提示、状态汇总或 readout 输入框。

#### Scenario: 切换品牌刷新型号

- **WHEN** 用户选择另一个 Camera brand
- **THEN** Camera model 列表只展示该品牌的数据库型号
- **AND** 未产生隐式的跨品牌型号选择

#### Scenario: 无读出数据型号可见但禁用

- **GIVEN** 一个型号没有任何数值 readout 数据
- **WHEN** 用户展开 Camera model 列表
- **THEN** 该型号仍可见
- **AND** 用户不能选择该型号

#### Scenario: 面板不暴露内部估算

- **GIVEN** 某 job 最终使用 half-frame readout fallback
- **WHEN** 用户查看手动相机区域
- **THEN** 界面仍只显示 Camera brand 和 Camera model

### Requirement: 手动相机选择 SHALL 长期回显但仅在资格成立时可交互

面板 SHALL 回显长期保存的品牌和型号。资格不成立时保存值 SHALL 保留但 UI 隐藏；恢复资格时，存在且可选的保存值 SHALL 自动回显。

#### Scenario: 资格恢复后回显保存值

- **GIVEN** 用户此前保存了有效品牌和型号
- **AND** 当前队列曾因可完整识别视频而隐藏手动相机 UI
- **WHEN** 队列恢复为所有视频均无完整相机标识
- **THEN** 两个选择器重新显示并回显保存值

#### Scenario: 数据库已移除保存型号

- **GIVEN** 保存的型号不再存在于活动 camera_db
- **WHEN** 面板重新显示手动相机 UI
- **THEN** 保存值不被静默替换成其他型号
- **AND** 用户必须显式选择一个可用型号后才可生效

