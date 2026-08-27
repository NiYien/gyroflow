## Why

渲染队列中的视频可能完全不携带可识别的相机品牌和型号；此时现有 Lens Group 即使填写了焦距，也因缺少 camera_db 提供的传感器比例而无法构建正确的 `fx/fy`，同时也无法取得帧读出时间。需要为“整批素材全部无法识别相机”的工作流提供一套队列专用、长期保存的手动相机选择，并完整复用 camera_db 的几何和读出时间规则。

## What Changes

- 在 Lens Group 面板中按严格的全队列条件自动显示一套全局 Camera brand / Camera model 选择；不新增用户可见开关。
- 长期保存品牌和型号，但只在队列非空且所有 video job 的原始品牌+型号均不完整时生效；任一 job 可完整识别时，整个手动覆盖立即失效并隐藏。
- 从当前生效的 camera_db（含运行时 lens-data 包）列出品牌和型号；没有任何数据库读出值的型号保留显示但禁用。
- 对每个队列 job 创建不污染原始检测结果的有效元数据覆盖，使用该 job 的分辨率、帧率、已有录制模式提示以及 L1-L6 对应焦距，按品牌规则计算 crop、`unit_pixel_focal_length`、`fx/fy` 和帧读出时间。
- 帧读出时间先走 camera_db 既有档位和回退；仍无结果时按原 NiYien Tool 语义选取最近标准帧率并使用 `500 / fps` 毫秒的半帧估算。
- 手动相机覆盖仅作用于渲染队列 job；主界面单独打开的视频不受影响，从队列 Play 时通过 job 工程数据呈现相同结果。
- 品牌/型号变化、队列资格变化或 camera_db 热更新时重算受影响 job；无效或缺失配置继续由现有缺数据门阻止处理。

## Capabilities

### New Capabilities

- `queue-manual-camera-selection`: 定义全队列资格判定、长期相机选择、原始/有效元数据隔离、camera_db 目录枚举及逐 job 相机参数解析。

### Modified Capabilities

- `lens-group-ui`: 增加严格条件显示的全局品牌/型号选择，并规定无额外开关、状态汇总或读出时间输入框。
- `batch-lens-apply`: 队列镜头 profile 构建与重应用在资格成立时使用手动相机有效元数据，并把计算后的矩阵和读出时间写入 job 工程状态。
- `batch-lens-missing-data-gate`: 有效的手动相机解析结果可以满足 `sensor` 缺数据条件；缺型号、失效型号或不可解析几何时仍须拦截。

## Impact

- 主要涉及 `src/rendering/render_queue.rs`、`src/core/niyien_lens_presets.rs`、`src/controller.rs` 与 `src/ui/menu/LensGroupConfig.qml`。
- camera_db 读取必须使用 `src/core/gyro_source/mod.rs::get_camera_db_path()` 解析出的当前活动目录，并复用 `telemetry_parser::camera_db::CameraDatabase` 的 model/crop/readout API；若现有公开接口不足，仅增加集中、可单测的解析辅助接口，不重解析视频、不修改品牌 parser 的自动检测路径。
- 新增设置键用于长期保存品牌和型号；不改变 `lens_group_configs_v1` 的六槽 schema，也不改变现有 `lens_group_manual_edit` 开关语义。
- 不修改主预览直接加载路径、原始 `FileMetadata.camera_identifier`、自动相机检测结果或 camera_db 数据文件。
