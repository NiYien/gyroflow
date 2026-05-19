// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

mod canon;
mod file_metadata;
mod imu_transforms;
mod sony;
pub mod splines;
pub use file_metadata::*;
pub use imu_transforms::*;
pub use sony::interpolate_mesh;

use nalgebra::*;
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::io::{Read, Seek};
use std::iter::zip;
use std::sync::{Arc, atomic::AtomicBool};
use telemetry_parser::tags_impl::{GetWithType, GroupId, TagId, TimeQuaternion, TimeVector3};
use telemetry_parser::{Input, InputOptions, TagFilter, util};

use crate::camera_identifier::CameraIdentifier;
use crate::stabilization_params::ReadoutDirection;

use super::imu_integration::*;
use super::smoothing::SmoothingAlgorithm;
use crate::StabilizationParams;

const DEG2RAD: f64 = std::f64::consts::PI / 180.0;

pub type Quat64 = UnitQuaternion<f64>;
pub type TimeIMU = telemetry_parser::util::IMUData;
pub type TimeQuat = BTreeMap<i64, Quat64>; // key is timestamp_us
pub type TimeVec = BTreeMap<i64, Vector3<f64>>; // key is timestamp_us

const SENSEFLOW_NIYIEN_INIT_QUAT_WINDOW_MS: f64 = 1200.0;
const SENSEFLOW_NIYIEN_RAD_GYRO_2000DPS: f32 = 0.001_065_264_4;
const SENSEFLOW_NIYIEN_RAD2DEG: f32 = 57.295_78;

fn scale_sony_frame_readout_time(
    frame_readout_time: Option<f64>,
    original_sample_rate: f64,
    sample_rate: f64,
) -> Option<f64> {
    let frame_readout_time = frame_readout_time?;
    if !frame_readout_time.is_finite() {
        return None;
    }
    if !original_sample_rate.is_finite()
        || original_sample_rate <= 0.0
        || !sample_rate.is_finite()
        || sample_rate <= 0.0
    {
        return Some(frame_readout_time);
    }
    let scaled = frame_readout_time / original_sample_rate * sample_rate;
    if scaled.is_finite() {
        Some(scaled)
    } else {
        Some(frame_readout_time)
    }
}

fn extract_init_quaternion(
    additional_data: Option<&serde_json::Value>,
) -> Option<(f64, f64, f64, f64)> {
    let quat = additional_data?.get("init quart")?.get("v")?;

    Some((
        quat.get("w")?.as_f64()?,
        quat.get("x")?.as_f64()?,
        quat.get("y")?.as_f64()?,
        quat.get("z")?.as_f64()?,
    ))
}

fn senseflow_gyro_range(additional_data: Option<&serde_json::Value>) -> f32 {
    additional_data
        .and_then(|x| x.get("gyro_range"))
        .and_then(|x| x.as_f64())
        .map(|x| x as f32)
        .filter(|x| *x > f32::EPSILON)
        .unwrap_or(1000.0)
}

fn senseflow_install_angles(additional_data: Option<&serde_json::Value>) -> (i32, i32, i32) {
    let Some(arr) = additional_data
        .and_then(|x| x.get("install_angle"))
        .and_then(|x| x.as_array())
    else {
        return (0, 0, 0);
    };
    if arr.len() != 3 {
        return (0, 0, 0);
    }
    let to_i32 = |idx: usize| -> Option<i32> { arr.get(idx)?.as_i64().map(|x| x as i32) };
    match (to_i32(0), to_i32(1), to_i32(2)) {
        (Some(pitch), Some(roll), Some(yaw)) => (pitch, roll, yaw),
        _ => (0, 0, 0),
    }
}

fn niyien_fast_inv_sqrt(x: f32) -> f32 {
    let x2 = x * 0.5;
    let mut y = x;
    let mut i = y.to_bits() as i32;
    i = 0x5f375a86 - (i >> 1);
    y = f32::from_bits(i as u32);
    y *= 1.5 - (x2 * y * y);
    y *= 1.5 - (x2 * y * y);
    y
}

#[derive(Clone, Debug)]
pub struct SenseFlowAutoRotationState {
    inited: bool,
    two_kp: f32,
    two_kp_fix: f32,
    q: [f32; 4],
    e_norm: f32,
    dot_e: f32,
    q0q0: f32,
    q0q1: f32,
    q0q2: f32,
    q1q1: f32,
    q1q3: f32,
    q2q2: f32,
    q2q3: f32,
    q3q3: f32,
    ex: f32,
    ey: f32,
    ez: f32,
}

impl Default for SenseFlowAutoRotationState {
    fn default() -> Self {
        Self {
            inited: false,
            two_kp: 10.0,
            two_kp_fix: 1.0,
            q: [1.0, 0.0, 0.0, 0.0],
            e_norm: 0.0,
            dot_e: 0.0,
            q0q0: 1.0,
            q0q1: 0.0,
            q0q2: 0.0,
            q1q1: 0.0,
            q1q3: 0.0,
            q2q2: 0.0,
            q2q3: 0.0,
            q3q3: 0.0,
            ex: 0.0,
            ey: 0.0,
            ez: 0.0,
        }
    }
}

impl SenseFlowAutoRotationState {
    pub fn reset_like_niyien(&mut self) {
        self.inited = false;
        self.two_kp = 15.0;
        self.e_norm = 1.0;
    }

    fn quat_normalize(&mut self) {
        let norm = niyien_fast_inv_sqrt(
            self.q[0] * self.q[0]
                + self.q[1] * self.q[1]
                + self.q[2] * self.q[2]
                + self.q[3] * self.q[3],
        );
        self.q[0] *= norm;
        self.q[1] *= norm;
        self.q[2] *= norm;
        self.q[3] *= norm;
    }

    fn quat_calc_dcm(&mut self) {
        self.q0q0 = self.q[0] * self.q[0];
        self.q0q1 = self.q[0] * self.q[1];
        self.q0q2 = self.q[0] * self.q[2];
        self.q1q1 = self.q[1] * self.q[1];
        self.q1q3 = self.q[1] * self.q[3];
        self.q2q2 = self.q[2] * self.q[2];
        self.q2q3 = self.q[2] * self.q[3];
        self.q3q3 = self.q[3] * self.q[3];
    }

    fn gyro_update(&mut self, gyro_raw_counts: [f32; 3]) {
        let half_t = 0.001_f32 * 0.5;
        let mut gx = gyro_raw_counts[0] * SENSEFLOW_NIYIEN_RAD_GYRO_2000DPS;
        let mut gy = gyro_raw_counts[1] * SENSEFLOW_NIYIEN_RAD_GYRO_2000DPS;
        let mut gz = gyro_raw_counts[2] * SENSEFLOW_NIYIEN_RAD_GYRO_2000DPS;

        if self.ex != 0.0 && self.ey != 0.0 && self.ez != 0.0 {
            gx += self.two_kp * self.ex;
            gy += self.two_kp * self.ey;
            gz += self.two_kp * self.ez;
        }

        gx *= half_t;
        gy *= half_t;
        gz *= half_t;

        let q = self.q;
        self.q[0] = q[0] + (-q[1] * gx - q[2] * gy - q[3] * gz);
        self.q[1] = q[1] + (q[0] * gx + q[2] * gz - q[3] * gy);
        self.q[2] = q[2] + (q[0] * gy - q[1] * gz + q[3] * gx);
        self.q[3] = q[3] + (q[0] * gz + q[1] * gy - q[2] * gx);
    }

    fn acc_update(&mut self, accl: [f32; 3]) {
        let mut ax = accl[0];
        let mut ay = accl[1];
        let mut az = accl[2];

        if !self.inited && self.e_norm < 0.00001 && self.dot_e > 0.2 {
            self.two_kp = self.two_kp_fix;
            self.inited = true;
        }

        if ax == 0.0 && ay == 0.0 && az == 0.0 {
            return;
        }

        self.quat_normalize();
        self.quat_calc_dcm();

        let acc_norm = ax * ax + ay * ay + az * az;
        let norm = niyien_fast_inv_sqrt(acc_norm);
        ax *= norm;
        ay *= norm;
        az *= norm;

        let vx = 2.0 * (self.q1q3 - self.q0q2);
        let vy = 2.0 * (self.q0q1 + self.q2q3);
        let vz = self.q0q0 - self.q1q1 - self.q2q2 + self.q3q3;

        self.ex = ay * vz - az * vy;
        self.ey = az * vx - ax * vz;
        self.ez = ax * vy - ay * vx;
        self.dot_e = ax * vx + ay * vy + az * vz;
        self.e_norm = self.ex * self.ex + self.ey * self.ey + self.ez * self.ez;
    }

    fn quaternion(&self) -> [f32; 4] {
        self.q
    }
}

#[derive(Clone, Copy, Debug)]
struct SenseFlowAutoRotationInfo {
    pitch_deg: f32,
    roll_deg: f32,
    pitch_quantized: i32,
    roll_quantized: i32,
    direction: i32,
    output_rotation: i32,
}

fn senseflow_auto_rotation_info_from_quat(
    quat: [f32; 4],
    install_angles: (i32, i32, i32),
) -> Option<SenseFlowAutoRotationInfo> {
    let [w, x, y, z] = quat;
    let norm_sq = w * w + x * x + y * y + z * z;
    if norm_sq <= f32::EPSILON {
        return None;
    }

    let dcm22 = 2.0 * (x * z - w * y);
    let dcm20 = -2.0 * (y * z + w * x);
    let dcm21 = -(1.0 - 2.0 * (x * x + y * y));

    let pitch_deg = dcm21.clamp(-1.0, 1.0).asin() * SENSEFLOW_NIYIEN_RAD2DEG;
    let roll_deg = dcm20.clamp(-1.0, 1.0).asin() * SENSEFLOW_NIYIEN_RAD2DEG;

    let pitch_quantized = if pitch_deg > 45.0 {
        90
    } else if pitch_deg < -45.0 {
        -90
    } else {
        0
    };

    let roll_quantized = if roll_deg > 45.0 {
        90
    } else if roll_deg < -45.0 {
        -90
    } else if dcm22 < -0.35 {
        -180
    } else {
        0
    };

    let mut direction = roll_quantized;
    if install_angles.2 == 90 {
        direction = -pitch_quantized;
    } else if install_angles.2 == -90 {
        direction = pitch_quantized;
    }

    let rotation = (direction - install_angles.1).rem_euclid(360);
    let output_rotation = if rotation < 45 || rotation > 315 {
        0
    } else if rotation < 135 {
        90
    } else if rotation < 225 {
        180
    } else {
        270
    };

    Some(SenseFlowAutoRotationInfo {
        pitch_deg,
        roll_deg,
        pitch_quantized,
        roll_quantized,
        direction,
        output_rotation,
    })
}

fn auto_rotation_from_init_quaternion(
    additional_data: Option<&serde_json::Value>,
) -> Option<SenseFlowAutoRotationInfo> {
    let (w, x, y, z) = extract_init_quaternion(additional_data)?;
    let install_angles = senseflow_install_angles(additional_data);
    senseflow_auto_rotation_info_from_quat([w as f32, x as f32, y as f32, z as f32], install_angles)
}

pub fn compute_auto_rotation(
    additional_data: Option<&serde_json::Value>,
    _raw_imu: &[TimeIMU],
    _duration_ms: f64,
    use_init_quat: bool,
) -> Option<i32> {
    if !use_init_quat {
        return None;
    }
    let info = auto_rotation_from_init_quaternion(additional_data)?;
    log::info!(
        "[auto_rotate niyien header] pitch={:.2} pitch_q={} roll={:.2} roll_q={} direction={} output_rotation={}",
        info.pitch_deg,
        info.pitch_quantized,
        info.roll_deg,
        info.roll_quantized,
        info.direction,
        info.output_rotation
    );
    Some(info.output_rotation)
}

pub fn compute_auto_rotation_for_segment_with_state(
    state: &mut SenseFlowAutoRotationState,
    raw_imu: &[TimeIMU],
    additional_data: Option<&serde_json::Value>,
    debug_label: &str,
) -> Option<i32> {
    if raw_imu.is_empty() {
        return None;
    }

    state.reset_like_niyien();

    let gyro_raw_scale = 32768.0_f32 / senseflow_gyro_range(additional_data);
    let first_ts = raw_imu.first().map(|x| x.timestamp_ms).unwrap_or_default();
    let end_ts = first_ts + SENSEFLOW_NIYIEN_INIT_QUAT_WINDOW_MS;
    let install_angles = senseflow_install_angles(additional_data);

    let mut used_samples = 0usize;
    for sample in raw_imu
        .iter()
        .take_while(|sample| sample.timestamp_ms <= end_ts)
    {
        let (Some(gyro), Some(accl)) = (sample.gyro, sample.accl) else {
            continue;
        };
        state.gyro_update([
            (gyro[0] as f32) * gyro_raw_scale,
            (gyro[1] as f32) * gyro_raw_scale,
            (gyro[2] as f32) * gyro_raw_scale,
        ]);
        state.acc_update([accl[0] as f32, accl[1] as f32, accl[2] as f32]);
        used_samples += 1;
    }

    if used_samples == 0 {
        return None;
    }

    let info = senseflow_auto_rotation_info_from_quat(state.quaternion(), install_angles)?;
    log::info!(
        "[auto_rotate niyien segment] file='{}' samples={} pitch={:.2} pitch_q={} roll={:.2} roll_q={} direction={} output_rotation={}",
        debug_label,
        used_samples,
        info.pitch_deg,
        info.pitch_quantized,
        info.roll_deg,
        info.roll_quantized,
        info.direction,
        info.output_rotation
    );
    Some(info.output_rotation)
}

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileLoadOptions {
    pub sample_index: Option<usize>,
    pub project_version: u64,
    #[serde(default)]
    pub header_only: bool,
    #[serde(default)]
    pub time_range_ms: Option<(f64, f64)>,
}

pub fn get_camera_db_path() -> Option<String> {
    // 1. Updated package in writable app data directory (lens hot-update bundle)
    if let Some(path) = crate::distribution::resolve_package_subdir("lens", "camera_db") {
        return Some(path.to_string_lossy().into_owned());
    }
    // 2. Bundled with the executable / app bundle
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // 2a. Windows / Linux: camera_db sitting next to the executable
            let p = dir.join("camera_db");
            if p.is_dir() {
                return Some(p.to_string_lossy().into_owned());
            }
            // 2b. macOS .app bundle: Contents/MacOS/Gyroflow -> ../Resources/camera_db
            let mac_p = dir.join("../Resources/camera_db");
            if mac_p.is_dir() {
                return Some(mac_p.to_string_lossy().into_owned());
            }
            // 2c. Dev layout: target/debug|release/camera_db copied by build.rs via resources/
            let resources_p = dir.join("resources/camera_db");
            if resources_p.is_dir() {
                return Some(resources_p.to_string_lossy().into_owned());
            }
        }
    }
    // 3. build.rs populated snapshot in the repo's resources/ directory (development)
    let build_snapshot =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/camera_db");
    if build_snapshot.is_dir() {
        return Some(build_snapshot.to_string_lossy().into_owned());
    }
    // 4. Legacy: telemetry-parser sibling checkout (historical fallback)
    let dev_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../telemetry-parser/camera_db");
    if dev_path.is_dir() {
        return Some(dev_path.to_string_lossy().into_owned());
    }
    None
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct GyroSource {
    pub file_load_options: FileLoadOptions,

    pub duration_ms: f64,

    raw_imu: Vec<TimeIMU>,

    pub imu_transforms: IMUTransforms,

    pub integration_method: usize,

    pub quaternions: TimeQuat,
    pub smoothed_quaternions: TimeQuat,

    pub use_gravity_vectors: bool,
    pub horizon_lock_integration_method: i32,

    pub max_angles: (f64, f64, f64), // (pitch, yaw, roll) in deg

    pub smoothing_status: serde_json::Value,

    pub prevent_recompute: bool,

    pub file_metadata: ReadOnlyFileMetadata, // Once this is set, it's never modified

    offsets: BTreeMap<i64, f64>, // <microseconds timestamp, offset in milliseconds>
    offsets_linear: BTreeMap<i64, f64>, // <microseconds timestamp, offset in milliseconds> - linear fit
    offsets_adjusted: BTreeMap<i64, f64>, // <timestamp + offset, offset>

    pub file_url: String,
}

impl GyroSource {
    pub fn new() -> Self {
        Self {
            integration_method: 2, // VQF
            use_gravity_vectors: false,
            horizon_lock_integration_method: 1, // VQF
            ..Default::default()
        }
    }

    pub fn has_motion(&self) -> bool {
        self.file_metadata.read().has_motion()
    }

    pub fn set_use_gravity_vectors(&mut self, v: bool) {
        if self.use_gravity_vectors != v {
            self.use_gravity_vectors = v;
            self.integrate();
        }
        self.use_gravity_vectors = v;
    }

    pub fn set_horizon_lock_integration_method(&mut self, v: i32) {
        if self.horizon_lock_integration_method != v {
            self.horizon_lock_integration_method = v;
            self.integrate();
        }
        self.horizon_lock_integration_method = v;
    }

    pub fn init_from_params(&mut self, stabilization_params: &StabilizationParams) {
        self.duration_ms = stabilization_params.get_scaled_duration_ms();
    }

    /// Trim all IMU data and quaternions to the given time range [start_us, end_us].
    /// Timestamps are in microseconds. If start > end, all data is cleared.
    pub fn trim_to_time_range(&mut self, start_us: i64, end_us: i64) {
        if start_us > end_us {
            self.raw_imu.clear();
            self.quaternions.clear();
            self.smoothed_quaternions.clear();
            self.duration_ms = 0.0;
            return;
        }

        let start_ms = start_us as f64 / 1000.0;
        let end_ms = end_us as f64 / 1000.0;

        // Filter raw_imu (timestamp_ms is f64, convert bounds to ms)
        self.raw_imu
            .retain(|sample| sample.timestamp_ms >= start_ms && sample.timestamp_ms <= end_ms);

        // Rebuild quaternions from BTreeMap range (keys are in microseconds)
        self.quaternions = self
            .quaternions
            .range(start_us..=end_us)
            .map(|(&k, &v)| (k, v))
            .collect();

        self.smoothed_quaternions = self
            .smoothed_quaternions
            .range(start_us..=end_us)
            .map(|(&k, &v)| (k, v))
            .collect();

        // Also trim file_metadata so that recompute/integrate won't restore untrimmed data
        {
            let mut fm = self.file_metadata.write();
            fm.raw_imu
                .retain(|sample| sample.timestamp_ms >= start_ms && sample.timestamp_ms <= end_ms);
            fm.quaternions = fm
                .quaternions
                .range(start_us..=end_us)
                .map(|(&k, &v)| (k, v))
                .collect();
            if let Some(ref mut gv) = fm.gravity_vectors {
                *gv = gv.range(start_us..=end_us).map(|(&k, &v)| (k, v)).collect();
            }
        }

        // Update duration
        self.duration_ms = end_ms - start_ms;
    }

    pub fn parse_telemetry_file<T: Read + Seek, P: AsRef<std::path::Path>, F: Fn(f64)>(
        stream: &mut T,
        filesize: usize,
        path: P,
        options: &FileLoadOptions,
        size: (usize, usize),
        fps: f64,
        progress_cb: F,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<FileMetadata, crate::GyroflowCoreError> {
        let t_total = std::time::Instant::now();
        // path comes from QML/QUrl callers as a file:// URL (sometimes percent-
        // encoded, sometimes not). Normalize to a native filesystem path so the
        // same file always hits the same cache slot, and so std::fs::metadata
        // can resolve mtime for invalidation when the file is overwritten.
        let path_str = path.as_ref().to_string_lossy().to_string();
        let native_path = crate::filesystem::url_to_path(&path_str);
        let mtime = std::fs::metadata(&native_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let key = format!("{}|{}|{}|{options:?}|{size:?}|{fps}", native_path, mtime, filesize);
        log::info!(
            "[parse_telemetry] begin path='{}' filesize={} mtime={} header_only={} time_range_ms={:?} size={:?} fps={:.6}",
            native_path,
            filesize,
            mtime,
            options.header_only,
            options.time_range_ms,
            size,
            fps
        );
        static CACHE: RwLock<BTreeMap<String, FileMetadata>> = RwLock::new(BTreeMap::new());
        {
            let cache = CACHE.read();
            if let Some(md) = cache.get(&key) {
                log::info!(
                    "[parse_telemetry] cache hit: {} (mtime={}, elapsed_ms={:.1})",
                    native_path,
                    mtime,
                    t_total.elapsed().as_secs_f64() * 1000.0
                );
                return Ok(md.clone());
            }
        }

        let camera_db_path = crate::gyro_source::get_camera_db_path();
        log::info!("camera_db_path: {:?}", camera_db_path);
        let tpoptions = InputOptions {
            blackbox_gyro_only: true,
            tag_blacklist: [
                TagFilter::EntireGroup(GroupId::UnknownGroup(0xf000)),
                TagFilter::EntireGroup(GroupId::UnknownGroup(0x0)),
            ]
            .into(),
            camera_db_path,
            header_only: options.header_only,
            time_range_ms: options.time_range_ms,
            ..Default::default()
        };
        let t_input = std::time::Instant::now();
        log::info!(
            "[parse_telemetry:input] begin path='{}' header_only={} time_range_ms={:?}",
            native_path,
            options.header_only,
            options.time_range_ms
        );
        let input_result = Input::from_stream_with_options(
            stream,
            filesize,
            &path,
            progress_cb,
            cancel_flag,
            tpoptions,
        );
        let mut input = match input_result {
            Ok(input) => {
                log::info!(
                    "[parse_telemetry:input] end path='{}' elapsed_ms={:.1} camera_type='{}' camera_model={:?} samples={}",
                    native_path,
                    t_input.elapsed().as_secs_f64() * 1000.0,
                    input.camera_type(),
                    input.camera_model(),
                    input.samples.as_ref().map(|s| s.len()).unwrap_or_default()
                );
                input
            }
            Err(e) => {
                log::warn!(
                    "[parse_telemetry:input] error path='{}' elapsed_ms={:.1} error={}",
                    native_path,
                    t_input.elapsed().as_secs_f64() * 1000.0,
                    e
                );
                return Err(e.into());
            }
        };

        let camera_identifier =
            CameraIdentifier::from_telemetry_parser(&input, size.0, size.1, fps).ok();

        let mut detected_source = input.camera_type();
        if let Some(m) = input.camera_model() {
            detected_source.push(' ');
            detected_source.push_str(m);
        }

        // C1: classify RED bodies — Komodo / Komodo-X are the only RED models
        // whose internal IMU we trust. Identification only here; C2/C3 will
        // act on the flag (clear non-Komodo IMU, prefer Komodo IMU over external).
        let is_komodo = input.camera_type() == "RED"
            && input
                .camera_model()
                .map(|m| m.starts_with("KOMODO"))
                .unwrap_or(false);
        if input.camera_type() == "RED" {
            log::info!(
                "[red_classify] type={} model={:?} is_komodo={}",
                input.camera_type(),
                input.camera_model(),
                is_komodo
            );
        }

        let mut imu_orientation = None;
        let mut quaternions = TimeQuat::default();
        let mut gravity_vectors: Option<TimeVec> = None;
        let mut image_orientations = None;
        let mut lens_profile = None;
        let mut frame_rate = None;
        let mut record_frame_rate = None;
        let mut digital_zoom = None;
        let mut lens_positions = BTreeMap::new();
        let mut lens_params = BTreeMap::new();
        let mut unit_pixel_focal_length = None;
        let mut additional_data = serde_json::Value::Object(serde_json::Map::new());

        if input.camera_type() == "BlackBox" {
            if let Some(ref mut samples) = input.samples {
                let mut usable_logs = Vec::new();
                for info in samples.iter() {
                    log::info!(
                        "Blackbox log #{}: Timestamp {:.3} | Duration {:.3} | Data: {}",
                        info.sample_index + 1,
                        info.timestamp_ms / 1000.0,
                        info.duration_ms / 1000.0,
                        info.tag_map.is_some()
                    );
                    if info.tag_map.is_some() && info.duration_ms > 0.0 {
                        usable_logs.push(serde_json::Value::String(format!(
                            "{};{};{}",
                            info.sample_index, info.timestamp_ms, info.duration_ms
                        )));
                    }
                }
                if let Some(requested_index) = options.sample_index {
                    samples.retain(|x| x.sample_index as usize == requested_index);
                }
                additional_data.as_object_mut().unwrap().insert(
                    "usable_logs".to_owned(),
                    serde_json::Value::Array(usable_logs),
                );
            }
        }

        // Get IMU orientation and quaternions
        if let Some(ref samples) = input.samples {
            let mut quats = TimeQuat::new();
            let mut grav = Vec::<Vector3<f64>>::new();
            let mut iori_map = TimeQuat::new();
            let mut iori = Vec::<Quat64>::new();
            let mut crop_score = Vec::<f64>::new();
            let mut grav_is_usable = false;
            let mut lens_info = LensParams::default();
            for info in samples {
                let timestamp_us = (info.timestamp_ms * 1000.0).round() as i64;
                if let Some(ref tag_map) = info.tag_map {
                    if let Some(map) = tag_map.get(&GroupId::Quaternion) {
                        if let Some(arr) =
                            map.get_t(TagId::Data) as Option<&Vec<TimeQuaternion<f64>>>
                        {
                            for v in arr {
                                quats.insert(
                                    (v.t * 1000.0) as i64,
                                    Quat64::from_quaternion(Quaternion::from_parts(
                                        v.v.w,
                                        Vector3::new(v.v.x, v.v.y, v.v.z),
                                    )),
                                );
                            }
                        }
                    }
                    if let Some(im) = tag_map.get(&GroupId::Imager) {
                        if input.camera_type() == "RED" {
                            lens_info.capture_area_size = Some((size.0 as f32, size.1 as f32));
                        }
                        if let Some(v) = im.get_t(TagId::PixelPitch) as Option<&(u32, u32)> {
                            lens_info.pixel_pitch = Some(*v);
                        }
                        if let Some(v) = im.get_t(TagId::CaptureAreaSize) as Option<&(f32, f32)> {
                            lens_info.capture_area_size = Some(*v);
                        }
                        if let Some(v) = im.get_t(TagId::CaptureAreaOrigin) as Option<&(f32, f32)> {
                            lens_info.capture_area_origin = Some(*v);
                        }
                        if let Some(v) = im.get_t(TagId::SensorSizePixels) as Option<&(u32, u32)> {
                            lens_info.sensor_size_px = Some(*v);
                        }
                        if let Some(w) = im.get_t(TagId::PixelWidth) as Option<&u32> {
                            if let Some(h) = im.get_t(TagId::PixelHeight) as Option<&u32> {
                                lens_info.capture_area_origin = Some((0.0, 0.0));
                                lens_info.sensor_size_px = Some((*w, *h));
                                lens_info.capture_area_size = Some((*w as f32, *h as f32));

                                if let Some(def) = tag_map.get(&GroupId::Default) {
                                    if let Some(sw) = def.get_t(TagId::SensorWidth) as Option<&f32>
                                    {
                                        if let Some(sh) =
                                            def.get_t(TagId::SensorHeight) as Option<&f32>
                                        {
                                            lens_info.pixel_pitch = Some((
                                                (*sw * 1000000.0 / *w as f32).round() as u32,
                                                (*sh * 1000000.0 / *h as f32).round() as u32,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(map) = tag_map.get(&GroupId::Lens) {
                        if let Some(v) = map.get_t(TagId::Data) as Option<&serde_json::Value> {
                            lens_profile = Some(v.clone());
                        }
                        if let Some(v) = map.get_t(TagId::Name) as Option<&String> {
                            lens_profile = Some(serde_json::Value::String(v.clone()));
                        }
                        if let Some(v) = map.get_t(TagId::FocalLength) as Option<&f32> {
                            if *v > 5.0 {
                                lens_positions.insert(timestamp_us, *v as f64);
                                lens_info.focal_length = Some(*v);
                            }
                        }
                        if let Some(v) = map.get_t(TagId::FocusDistance) as Option<&f32> {
                            lens_info.focus_distance = Some(*v);
                        }
                        if let Some(v) = map.get_t(TagId::PixelFocalLength) as Option<&f32> {
                            if *v > 5.0 {
                                lens_info.pixel_focal_length = Some(*v);
                            }
                        }
                        if let Some(v) = map.get_t(TagId::PixelFocalLength) as Option<&Vec<f32>> {
                            if let Some(v) = v.first() {
                                if *v > 5.0 {
                                    lens_info.pixel_focal_length = Some(*v);
                                }
                            }
                        }
                        if let Some(v) = map.get_t(TagId::Custom("unit_pixel_focal_length".into()))
                            as Option<&f64>
                        {
                            unit_pixel_focal_length = Some(*v);
                        }
                    }
                    if lens_info.focal_length.is_none() {
                        if let Some(md) = tag_map.get(&GroupId::Custom("LensDistortion".into())) {
                            if let Some(v) = md.get_t(TagId::Data) as Option<&serde_json::Value> {
                                // lens.focal_length = v.get("focal_length_nm").and_then(|x| x.as_f64()).map(|x| (x / 1000000.0) as f32);
                                let focal_length_nm = v
                                    .get("focal_length_nm")
                                    .and_then(|x| x.as_f64())
                                    .unwrap_or_default();
                                let effective_sensor_height_nm = v
                                    .get("effective_sensor_height_nm")
                                    .and_then(|x| x.as_f64())
                                    .unwrap_or(1.0);

                                let focal_length_mm = focal_length_nm / 1_000_000.0;
                                let pixel_focal_length = if focal_length_mm > 5.0
                                    && effective_sensor_height_nm > 0.0
                                {
                                    ((focal_length_nm / effective_sensor_height_nm)
                                        * size.1 as f64) as f32
                                } else {
                                    0.0
                                };
                                if pixel_focal_length > 5.0 {
                                    lens_info.pixel_focal_length = Some(pixel_focal_length);
                                }
                            }
                        }
                    }
                    if lens_info.pixel_focal_length.is_some()
                        || (lens_info.pixel_pitch.is_some()
                            && lens_info.capture_area_size.is_some()
                            && lens_info.focal_length.is_some())
                    {
                        lens_params.insert(timestamp_us, lens_info.clone());
                    }

                    if let Some(map) = tag_map.get(&GroupId::Default) {
                        if let Some(v) = map.get_t(TagId::FrameRate) as Option<&f64> {
                            frame_rate = Some(*v);
                        }
                        if let Some(v) = map.get_t(TagId::RecordFrameRate) as Option<&f64> {
                            record_frame_rate = Some(*v);
                        }
                        if let Some(v) = map.get_t(TagId::ImageStabilizer) as Option<&bool> {
                            additional_data.as_object_mut().map(|o| {
                                o.insert("image_stabilizer".to_owned(), serde_json::Value::Bool(*v))
                            });
                        }
                        if let Some(v) = map.get_t(TagId::Metadata) as Option<&serde_json::Value> {
                            crate::util::merge_json(&mut additional_data, v);
                        }
                    }
                    if let Some(map) = tag_map.get(&GroupId::Custom("FovAdaptationScore".into())) {
                        if let Some(v) = map.get_t(TagId::Data) as Option<&Vec<f32>> {
                            for v in v {
                                crop_score.push(*v as f64);
                            }
                        }
                    }
                    if let Some(map) = tag_map.get(&GroupId::Default) {
                        if let Some(v) =
                            map.get_t(TagId::Unknown(0x445a5354 /*DZST*/)) as Option<&u32>
                        {
                            if *v != 0 {
                                let max = *(map.get_t(TagId::Unknown(0x445a4d58 /*DZMX*/))
                                    as Option<&f32>)
                                    .unwrap_or(&1.4)
                                    as f64;
                                digital_zoom = Some(1.0 + (*v as f64 / 100.0) * (max - 1.0));
                            }
                        }
                    }
                    if let Some(map) = tag_map.get(&GroupId::GravityVector) {
                        let scale =
                            *(map.get_t(TagId::Scale) as Option<&i16>).unwrap_or(&32767) as f64;
                        if scale > 0.0 {
                            if let Some(arr) = map.get_t(TagId::Data)
                                as Option<&Vec<telemetry_parser::tags_impl::Vector3<i16>>>
                            {
                                for v in arr {
                                    if v.x != 0 || v.y != 0 || v.z != 0 {
                                        grav_is_usable = true;
                                    }
                                    grav.push(Vector3::new(
                                        v.x as f64 / scale,
                                        v.y as f64 / scale,
                                        v.z as f64 / scale,
                                    ));
                                }
                            }
                        }
                    }
                    if let Some(map) = tag_map.get(&GroupId::Gyroscope) {
                        let mut io = match map.get_t(TagId::Orientation) as Option<&String> {
                            Some(v) if v.len() == 3 => v.clone(),
                            _ => "XYZ".into(),
                        };
                        io = input.normalize_imu_orientation(io);
                        imu_orientation = Some(io);
                    }
                    if let Some(map) = tag_map.get(&GroupId::ImageOrientation) {
                        let scale =
                            *(map.get_t(TagId::Scale) as Option<&i16>).unwrap_or(&32767) as f64;
                        if let Some(arr) = map.get_t(TagId::Data)
                            as Option<&Vec<telemetry_parser::tags_impl::Quaternion<i16>>>
                        {
                            for v in arr.iter() {
                                iori.push(Quat64::from_quaternion(
                                    nalgebra::Quaternion::<f64>::from_vector(Vector4::new(
                                        v.x as f64 / scale,
                                        v.y as f64 / scale,
                                        v.z as f64 / scale,
                                        v.w as f64 / scale,
                                    )),
                                ));
                            }
                        }
                    }
                    let additional_data = additional_data.as_object_mut().unwrap();
                    if !additional_data.contains_key("recording_settings") {
                        let mut settings = serde_json::Map::new();
                        if let Some(map) = tag_map.get(&GroupId::Exposure) {
                            if let Some(v) = map.get(&TagId::ShutterAngle) {
                                settings.insert(
                                    String::from("Shutter angle"),
                                    v.value.to_string().into(),
                                );
                            }
                            if let Some(v) = map.get(&TagId::ShutterSpeed) {
                                settings.insert(
                                    String::from("Shutter speed"),
                                    v.value.to_string().into(),
                                );
                            }
                            if let Some(v) = map.get(&TagId::AutoExposureMode) {
                                settings
                                    .insert(String::from("Exposure"), v.value.to_string().into());
                            }
                            if let Some(v) = map.get(&TagId::Custom("ISOValue3".into())) {
                                settings.insert(String::from("ISO"), v.value.to_string().into());
                            } else if let Some(v) = map.get(&TagId::ISOValue) {
                                settings.insert(String::from("ISO"), v.value.to_string().into());
                            }
                        }
                        if let Some(map) = tag_map.get(&GroupId::Colors) {
                            if let Some(v) = map.get(&TagId::ColorPrimaries) {
                                settings.insert(
                                    String::from("Color primaries"),
                                    v.value.to_string().into(),
                                );
                            }
                            if let Some(v) = map.get(&TagId::CaptureGammaEquation) {
                                settings.insert(
                                    String::from("Gamma equation"),
                                    v.value.to_string().into(),
                                );
                            }
                            if let Some(v) = map.get(&TagId::AutoWBMode) {
                                settings.insert(
                                    String::from("White balance mode"),
                                    v.value.to_string().into(),
                                );
                            }
                            if let Some(v) = map.get(&TagId::WhiteBalance) {
                                settings.insert(
                                    String::from("White balance"),
                                    v.value.to_string().into(),
                                );
                            }
                        }
                        if let Some(map) = tag_map.get(&GroupId::Lens) {
                            if let Some(v) = map.get(&TagId::IrisTStop) {
                                settings.insert(String::from("Iris"), v.value.to_string().into());
                            } else if let Some(v) = map.get(&TagId::IrisFStop) {
                                settings.insert(String::from("Iris"), v.value.to_string().into());
                            }
                            if let Some(v) = map.get(&TagId::FocalLength) {
                                settings.insert(
                                    String::from("Focal length"),
                                    v.value.to_string().into(),
                                );
                            }
                        }
                        if let Some(map) = tag_map.get(&GroupId::Autofocus) {
                            if let Some(v) = map.get(&TagId::AutoFocusMode) {
                                settings
                                    .insert(String::from("Focus mode"), v.value.to_string().into());
                            }
                        }
                        if !settings.is_empty() {
                            additional_data.insert(
                                "recording_settings".to_owned(),
                                serde_json::Value::Object(settings),
                            );
                        }
                    }
                }
            }

            if !grav_is_usable {
                grav.clear();
            }

            for ((ts, _quat), iori) in zip(&quats, &iori) {
                iori_map.insert(*ts, *iori);
            }
            if !iori_map.is_empty() {
                image_orientations = Some(iori_map);
            }

            if !quats.is_empty() {
                if !grav.is_empty() && grav.len() == quats.len() {
                    if grav.len() == iori.len() {
                        for (g, q) in grav.iter_mut().zip(iori.iter()) {
                            *g = (*q) * (*g);
                        }
                    }

                    gravity_vectors = Some(quats.keys().copied().zip(grav.into_iter()).collect());
                }

                if lens_positions.is_empty()
                    && !crop_score.is_empty()
                    && crop_score.len() == quats.len()
                {
                    lens_positions = quats
                        .iter()
                        .zip(crop_score.iter())
                        .map(|((ts, _), crop)| (*ts, *crop))
                        .collect();
                }

                quaternions = quats;
            }
        }

        let mut raw_imu =
            util::normalized_imu_interpolated(&input, Some("XYZ".into())).unwrap_or_default();

        if (input.camera_type() == "RED" || input.camera_type() == "RED RAW")
            && options.project_version > 0
            && options.project_version < 4
        {
            // Legacy gyro offset
            let mut first_timestamp = None;
            log::debug!("Legacy project, removing new RED gyro offset");
            for x in raw_imu.iter_mut() {
                if first_timestamp.is_none() {
                    first_timestamp = Some(x.timestamp_ms);
                }
                x.timestamp_ms -= first_timestamp.unwrap();
            }
        }
        let mut has_accurate_timestamps = input.has_accurate_timestamps();
        if let serde_json::Value::Object(o) = &mut additional_data {
            match o.get("has_accurate_timestamps") {
                Some(serde_json::Value::String(x)) => {
                    if x == "true" || x == "1" {
                        has_accurate_timestamps = true;
                    }
                }
                Some(serde_json::Value::Bool(x)) => {
                    if *x {
                        has_accurate_timestamps = true;
                    }
                }
                _ => {}
            }
            o.remove("has_accurate_timestamps");
        }

        let fr = input.frame_readout_time().unwrap_or_default();
        let frame_readout_time = if fr != 0.0 {
            Some(if fr.abs() > 10000.0 {
                fr.abs() - 10000.0
            } else {
                fr.abs()
            })
        } else {
            None
        };

        // Extract creation date/timezone from telemetry
        let mut creation_date = None;
        let mut timezone_offset = None;
        let mut creation_date_utc = None;
        if let Some(ref mut samples) = input.samples {
            if let Some(info) = samples.first() {
                if let Some(ref tag_map) = info.tag_map {
                    if let Some(map) = tag_map.get(&GroupId::Default) {
                        if let Some(v) = map.get_t(TagId::CreationDate) as Option<&String> {
                            creation_date = Some(v.clone());
                        }
                        if let Some(v) = map.get_t(TagId::TimeZoneOffset) as Option<&String> {
                            timezone_offset = Some(v.clone());
                        }
                        if let Some(v) = map.get_t(TagId::CreationDateUtc) as Option<&String> {
                            creation_date_utc = Some(v.clone());
                        }
                    }
                }
            }
        }

        let mut md = FileMetadata {
            imu_orientation,
            detected_source: Some(detected_source),
            is_komodo,
            quaternions,
            image_orientations,
            gravity_vectors,
            lens_positions,
            lens_params,
            raw_imu,
            frame_readout_time,
            frame_readout_direction: if fr < 0.0 {
                if fr.abs() > 10000.0 {
                    ReadoutDirection::RightToLeft
                } else {
                    ReadoutDirection::BottomToTop
                }
            } else {
                if fr.abs() > 10000.0 {
                    ReadoutDirection::LeftToRight
                } else {
                    ReadoutDirection::TopToBottom
                }
            },
            frame_rate,
            record_frame_rate,
            lens_profile,
            camera_identifier,
            has_accurate_timestamps,
            creation_date,
            timezone_offset,
            creation_date_utc,
            additional_data,
            per_frame_time_offsets: Vec::new(),
            unit_pixel_focal_length,
            digital_zoom,
            camera_stab_data: Vec::new(),
            mesh_correction: Vec::new(),
            duration_ms: input
                .samples
                .as_ref()
                .and_then(|s| s.first())
                .map(|s| s.duration_ms)
                .unwrap_or(0.0),
        };

        log::info!(
            "Telemetry parsed: lens_params={}, lens_positions={}, unit_px_fl={:?}, frame_readout_time={:?}, detected={}, camera_id={:?}",
            md.lens_params.len(),
            md.lens_positions.len(),
            md.unit_pixel_focal_length,
            md.frame_readout_time,
            md.detected_source.as_deref().unwrap_or("?"),
            md.camera_identifier
                .as_ref()
                .map(|c| format!("{} {}", c.brand, c.model))
        );
        if let Some((_ts, lp)) = md.lens_params.iter().next() {
            log::info!(
                "First lens_param: pixel_focal_length={:?}, focal_length={:?}, pixel_pitch={:?}",
                lp.pixel_focal_length,
                lp.focal_length,
                lp.pixel_pitch
            );
        }

        if md
            .detected_source
            .as_deref()
            .map(|source| source.starts_with("SenseFlow"))
            .unwrap_or(false)
        {
            let auto_rotation =
                compute_auto_rotation(Some(&md.additional_data), &md.raw_imu, md.duration_ms, true);

            if let Some(rotation) = auto_rotation {
                if let serde_json::Value::Object(o) = &mut md.additional_data {
                    o.insert("auto_rotation_deg".into(), rotation.into());
                }
            }
        }

        let sample_rate = Self::get_sample_rate(&md);
        let mut original_sample_rate = sample_rate;
        let mut is_temp = sony::ISTemp::default();
        let mut mesh_cache = BTreeMap::new();
        if let Some(ref samples) = input.samples {
            for info in samples {
                if let Some(ref tag_map) = info.tag_map {
                    // --------------------------------- Sony ---------------------------------
                    if let Some((org_sample_rate, offset)) =
                        sony::get_time_offset(&md, &input, tag_map, sample_rate)
                    {
                        original_sample_rate = org_sample_rate;
                        md.per_frame_time_offsets.push(offset);
                    }
                    sony::init_lens_profile(&mut md, &input, tag_map, size, info);
                    sony::stab_collect(&mut is_temp, tag_map, info, fps);
                    if let Some(mesh) = sony::get_mesh_correction(tag_map, &mut mesh_cache) {
                        md.mesh_correction.push(mesh);
                    }

                    if let Some(ois) = tag_map
                        .get(&GroupId::LensOSS)
                        .and_then(|x| x.get_t(TagId::Data) as Option<&Vec<TimeVector3<i32>>>)
                    {
                        if ois.len() == 1
                            && *ois.first().unwrap()
                                == (TimeVector3 {
                                    t: -1,
                                    x: -1,
                                    y: -1,
                                    z: -1,
                                })
                        {
                            if let serde_json::Value::Object(o) = &mut md.additional_data {
                                o.insert("unsupported_lens".into(), true.into());
                            }
                        }
                    }

                    // --------------------------------- Sony ---------------------------------

                    // --------------------------------- Sony ---------------------------------

                    // --------------------------------- RED ---------------------------------
                    if input.camera_type() == "RED" || input.camera_type() == "RED RAW" {
                        telemetry_parser::try_block!({
                            let legacy_offset =
                                options.project_version > 0 && options.project_version < 4;
                            if !legacy_offset {
                                let exposure_time =
                                    (tag_map.get(&GroupId::Default)?.get_t(TagId::ExposureTime)
                                        as Option<&f32>)?;
                                md.per_frame_time_offsets
                                    .push(-(*exposure_time as f64 / 1000.0) / 2.0);
                            }
                        });
                    }
                    // --------------------------------- RED ---------------------------------

                    // --------------------------------- Canon ---------------------------------
                    if input.camera_type() == "Canon" {
                        if let Some(offset) =
                            canon::get_time_offset(&md, &input, tag_map, sample_rate, fps)
                        {
                            md.per_frame_time_offsets.push(offset);
                        }
                        canon::init_lens_profile(&mut md, &input, tag_map, size, info);
                    }
                    // --------------------------------- Canon ---------------------------------

                    // --------------------------------- Insta360 ---------------------------------
                    // Timing
                    if input.camera_type() == "Insta360" {
                        telemetry_parser::try_block!({
                            use telemetry_parser::tags_impl::TimeScalar;
                            let exp = (tag_map.get(&GroupId::Exposure)?.get_t(TagId::Data)
                                as Option<&Vec<TimeScalar<f64>>>)?;
                            let tm = (tag_map
                                .get(&GroupId::Default)?
                                .get_t(TagId::Custom("TimeMap".into()))
                                as Option<&Vec<TimeScalar<f64>>>)
                                .cloned()
                                .unwrap_or_default();

                            let mut video_ts = 0.0;
                            let mut zero_ref = None;
                            let mut prev_t = 0.0;
                            let mut i = 0;
                            for x in exp {
                                if x.t > prev_t || x.t == 0.0 {
                                    if zero_ref.is_none() {
                                        zero_ref = Some(x.t * 1000.0);
                                        log::debug!(
                                            "Insta360 first frame reference time: {:.4}",
                                            x.t * 1000.0
                                        );
                                    }
                                    let tm_diff =
                                        tm.get(i).map(|tm| tm.t - tm.v).unwrap_or_default();

                                    // The additional 0.9 ms is a mystery
                                    let diff = (video_ts - x.t) * 1000.0;

                                    md.per_frame_time_offsets.push(
                                        -(x.v * 1000.0 / 2.0)
                                            - 0.9
                                            - diff
                                            - tm_diff
                                            - zero_ref.unwrap(),
                                    );

                                    video_ts += 1.0 / fps;
                                    prev_t = x.t;
                                    i += 1;
                                }
                            }
                        });
                    }
                    // --------------------------------- Insta360 ---------------------------------
                }
            }
            if input.camera_type() == "Sony" {
                md.camera_stab_data =
                    sony::stab_calc_splines(&md, &is_temp, sample_rate, fps, size)
                        .unwrap_or_default();
                if md.frame_readout_time.is_some() {
                    md.frame_readout_time = scale_sony_frame_readout_time(
                        md.frame_readout_time,
                        original_sample_rate,
                        sample_rate,
                    );
                }
            }
        }

        // [lens_diag] Part A2 diagnostic — print everything that feeds
        // should_use_manual_config / lens_params status on every parse_telemetry_file
        // call. The same parse path runs for both main video and external IMU/gyro
        // files, so this surfaces niyien gyro vs video metadata distinctions.
        {
            let path_str = path.as_ref().display().to_string();
            let camera_type = input.camera_type();
            let camera_model = input.camera_model().map(|s| s.to_string());
            let detected_source_str = md.detected_source.clone().unwrap_or_default();
            let lens_params_count = md.lens_params.len();
            let first_lens = md.lens_params.values().next();
            let first_focal = first_lens.and_then(|lp| lp.focal_length);
            let first_pixel_focal = first_lens.and_then(|lp| lp.pixel_focal_length);
            let upfl = md.unit_pixel_focal_length;
            let cam_id_brand = md
                .camera_identifier
                .as_ref()
                .map(|c| c.brand.clone())
                .unwrap_or_default();
            let cam_id_model = md
                .camera_identifier
                .as_ref()
                .map(|c| c.model.clone())
                .unwrap_or_default();
            let cam_id_focal = md.camera_identifier.as_ref().and_then(|c| c.focal_length);
            let additional_focus = md
                .additional_data
                .get("focus_length")
                .and_then(|v| v.as_f64());
            let additional_lens_index = md
                .additional_data
                .get("lens_index")
                .and_then(|v| v.as_u64());
            log::info!(
                "[lens_diag] path={path_str} camera_type={camera_type:?} camera_model={camera_model:?} detected_source={detected_source_str:?} cam_id=(brand={cam_id_brand:?} model={cam_id_model:?} focal={cam_id_focal:?}) lens_params_count={lens_params_count} first_focal={first_focal:?} first_pixel_focal={first_pixel_focal:?} unit_pixel_focal_length={upfl:?} additional_data.focus_length={additional_focus:?} additional_data.lens_index={additional_lens_index:?}"
            );
        }

        // C2: drop internal IMU on non-Komodo RED bodies so downstream external
        // IMU loading (telemetry sidecar / .bin) is the sole motion source.
        // Lens metadata (lens_params, lens_positions, camera_identifier,
        // unit_pixel_focal_length, frame_readout_time, etc.) is preserved.
        if input.camera_type() == "RED" && !is_komodo {
            let dropped_imu = md.raw_imu.len();
            let dropped_quat = md.quaternions.len();
            let dropped_grav = md.gravity_vectors.as_ref().map(|v| v.len()).unwrap_or(0);
            let dropped_iori = md.image_orientations.as_ref().map(|v| v.len()).unwrap_or(0);
            md.raw_imu.clear();
            md.quaternions.clear();
            md.gravity_vectors = None;
            md.image_orientations = None;
            md.imu_orientation = None;
            log::info!(
                "[red_filter] non-komodo RED ({:?}), dropped imu={} quat={} grav={} iori={}; lens metadata kept",
                input.camera_model(),
                dropped_imu,
                dropped_quat,
                dropped_grav,
                dropped_iori
            );
        }

        #[cfg(feature = "cache-gyro-metadata")]
        {
            log::info!("[parse_telemetry] cache miss \u{2192} parsed and cached: {} (mtime={})", native_path, mtime);
            let mut cache = CACHE.write();
            cache.insert(key, md.clone());
        }

        log::info!(
            "[parse_telemetry] end path='{}' elapsed_ms={:.1} raw_imu={} quats={} lens_params={} lens_positions={} duration_ms={:.3} creation_date_utc={:?} accurate_ts={} detected={:?} is_komodo={}",
            native_path,
            t_total.elapsed().as_secs_f64() * 1000.0,
            md.raw_imu.len(),
            md.quaternions.len(),
            md.lens_params.len(),
            md.lens_positions.len(),
            md.duration_ms,
            md.creation_date_utc,
            md.has_accurate_timestamps,
            md.detected_source,
            md.is_komodo
        );
        Ok(md)
    }

    pub fn clear(&mut self) {
        self.quaternions.clear();
        self.smoothed_quaternions.clear();
        self.raw_imu.clear();
        self.imu_transforms.imu_rotation = None;
        self.imu_transforms.acc_rotation = None;
        self.imu_transforms.imu_lpf = 0.0;
        self.imu_transforms.imu_mf = 0;
        self.file_metadata = Default::default();
        self.clear_offsets();
    }

    pub fn load_from_telemetry(&mut self, telemetry: FileMetadata) {
        if self.duration_ms <= 0.0 {
            ::log::error!("Invalid duration_ms {}", self.duration_ms);
            return;
        }

        self.clear();

        self.imu_transforms.imu_orientation = telemetry.imu_orientation.clone();

        let has_quats = !telemetry.quaternions.is_empty();
        let has_raw_imu = !telemetry.raw_imu.is_empty();

        self.file_metadata = telemetry.into();

        if has_quats {
            let file_metadata = self.file_metadata.read();
            self.quaternions = file_metadata.quaternions.clone();
            self.integration_method = 0;
            let len = file_metadata.quaternions.len() as f64;
            let first_ts = file_metadata
                .quaternions
                .iter()
                .next()
                .map(|x| *x.0 as f64 / 1000.0)
                .unwrap_or_default();
            let last_ts = file_metadata
                .quaternions
                .iter()
                .next_back()
                .map(|x| *x.0 as f64 / 1000.0)
                .unwrap_or_default();
            let imu_duration = (last_ts - first_ts) * ((len + 1.0) / len);
            if (imu_duration - self.duration_ms).abs() > 0.01 {
                if imu_duration > 0.0 {
                    self.duration_ms = imu_duration;
                }
            }
        }

        if has_raw_imu {
            {
                let file_metadata = self.file_metadata.read();
                let len = file_metadata.raw_imu.len() as f64;
                let first_ts = file_metadata
                    .raw_imu
                    .first()
                    .map(|x| x.timestamp_ms)
                    .unwrap_or_default();
                let last_ts = file_metadata
                    .raw_imu
                    .last()
                    .map(|x| x.timestamp_ms)
                    .unwrap_or_default();
                let imu_duration = (last_ts - first_ts) * ((len + 1.0) / len);
                if (imu_duration - self.duration_ms).abs() > 0.01 {
                    if imu_duration > 0.0 {
                        self.duration_ms = imu_duration;
                    }
                }
            }
            self.apply_transforms();
        } else if self.quaternions.is_empty() {
            self.integrate();
        }
    }
    pub fn integrate(&mut self) {
        // Entry guard: degenerate state from a concurrent project load (e.g.
        // duration_ms reset to 0 by init_from_params) MUST NOT trigger
        // integrator math (vqf.rs asserts gyr_ts > 0). Preserve existing
        // quaternions so downstream `recompute_smoothness` / rendering keep
        // operating on the last good state.
        if self.duration_ms <= 0.0 || !self.duration_ms.is_finite() {
            // No-video-loaded is a *normal* state during app startup and
            // between video loads, so the entry-guard hit is only worth a
            // Warn when we previously had quaternions (race-with-load case).
            // Empty-quaternions + degenerate duration is the expected
            // pre-load state — log at Debug to keep the noise out.
            if self.quaternions.is_empty() {
                log::debug!(
                    target: "lifecycle",
                    "GyroSource::integrate skipped: no video loaded yet (duration_ms={})",
                    self.duration_ms
                );
            } else {
                log::warn!(
                    target: "lifecycle",
                    "GyroSource::integrate skipped (degenerate state, preserving {} quaternions): duration_ms={}",
                    self.quaternions.len(),
                    self.duration_ms
                );
            }
            return;
        }
        let file_metadata = self.file_metadata.read();
        match self.integration_method {
            0 => {
                // Arm 0 (GoPro path) does NOT use the intermediate+conditional
                // pattern: the outer `if` already requires
                // `!file_metadata.quaternions.is_empty()` before invoking
                // QuaternionConverter::convert, and convert iterates over
                // org_quaternions producing one entry per input (falling back
                // to UnitQuaternion::identity() when integrated_quats is empty).
                // So the result is always non-empty for valid input — direct
                // assignment is safe here.
                self.quaternions = if file_metadata
                    .detected_source
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("GoPro")
                    && !file_metadata.quaternions.is_empty()
                    && (file_metadata.gravity_vectors.is_none() || !self.use_gravity_vectors)
                {
                    log::info!("No gravity vectors - using accelerometer");
                    QuaternionConverter::convert(
                        self.horizon_lock_integration_method,
                        &file_metadata.quaternions,
                        file_metadata
                            .image_orientations
                            .as_ref()
                            .unwrap_or(&TimeQuat::default()),
                        self.raw_imu(&file_metadata),
                        self.duration_ms,
                    )
                } else {
                    file_metadata.quaternions.clone()
                };
                if self.imu_transforms.imu_lpf > 0.0
                    && !self.quaternions.is_empty()
                    && self.duration_ms > 0.0
                {
                    let sample_rate = self.quaternions.len() as f64 / (self.duration_ms / 1000.0);
                    if let Err(e) = super::filtering::Lowpass::filter_quats_forward_backward(
                        self.imu_transforms.imu_lpf,
                        sample_rate,
                        &mut self.quaternions,
                    ) {
                        log::error!("Filter error {:?}", e);
                    }
                }
                if let Some(rot) = self.imu_transforms.imu_rotation {
                    for (_ts, q) in &mut self.quaternions {
                        *q = rot * *q;
                    }
                }
            }
            1 => {
                let q = ComplementaryIntegrator::integrate(
                    self.raw_imu(&file_metadata),
                    self.duration_ms,
                );
                if !q.is_empty() {
                    self.quaternions = q;
                } else if self.quaternions.is_empty() {
                    log::info!(
                        target: "lifecycle",
                        "ComplementaryIntegrator returned empty (cold start, no quaternions to preserve)"
                    );
                } else {
                    log::warn!(
                        target: "lifecycle",
                        "ComplementaryIntegrator returned empty, preserving existing {} quaternions",
                        self.quaternions.len()
                    );
                }
            }
            2 => {
                let q = VQFIntegrator::integrate(self.raw_imu(&file_metadata), self.duration_ms);
                if !q.is_empty() {
                    self.quaternions = q;
                } else if self.quaternions.is_empty() {
                    log::info!(
                        target: "lifecycle",
                        "VQFIntegrator returned empty (cold start, no quaternions to preserve)"
                    );
                } else {
                    log::warn!(
                        target: "lifecycle",
                        "VQFIntegrator returned empty, preserving existing {} quaternions",
                        self.quaternions.len()
                    );
                }
            }
            3 => {
                let q = SimpleGyroIntegrator::integrate(
                    self.raw_imu(&file_metadata),
                    self.duration_ms,
                );
                if !q.is_empty() {
                    self.quaternions = q;
                } else if self.quaternions.is_empty() {
                    log::info!(
                        target: "lifecycle",
                        "SimpleGyroIntegrator returned empty (cold start, no quaternions to preserve)"
                    );
                } else {
                    log::warn!(
                        target: "lifecycle",
                        "SimpleGyroIntegrator returned empty, preserving existing {} quaternions",
                        self.quaternions.len()
                    );
                }
            }
            4 => {
                let q = SimpleGyroAccelIntegrator::integrate(
                    self.raw_imu(&file_metadata),
                    self.duration_ms,
                );
                if !q.is_empty() {
                    self.quaternions = q;
                } else if self.quaternions.is_empty() {
                    log::info!(
                        target: "lifecycle",
                        "SimpleGyroAccelIntegrator returned empty (cold start, no quaternions to preserve)"
                    );
                } else {
                    log::warn!(
                        target: "lifecycle",
                        "SimpleGyroAccelIntegrator returned empty, preserving existing {} quaternions",
                        self.quaternions.len()
                    );
                }
            }
            5 => {
                let q = MahonyIntegrator::integrate(self.raw_imu(&file_metadata), self.duration_ms);
                if !q.is_empty() {
                    self.quaternions = q;
                } else if self.quaternions.is_empty() {
                    log::info!(
                        target: "lifecycle",
                        "MahonyIntegrator returned empty (cold start, no quaternions to preserve)"
                    );
                } else {
                    log::warn!(
                        target: "lifecycle",
                        "MahonyIntegrator returned empty, preserving existing {} quaternions",
                        self.quaternions.len()
                    );
                }
            }
            6 => {
                let q =
                    MadgwickIntegrator::integrate(self.raw_imu(&file_metadata), self.duration_ms);
                if !q.is_empty() {
                    self.quaternions = q;
                } else if self.quaternions.is_empty() {
                    log::info!(
                        target: "lifecycle",
                        "MadgwickIntegrator returned empty (cold start, no quaternions to preserve)"
                    );
                } else {
                    log::warn!(
                        target: "lifecycle",
                        "MadgwickIntegrator returned empty, preserving existing {} quaternions",
                        self.quaternions.len()
                    );
                }
            }
            _ => log::error!("Unknown integrator"),
        }
    }

    pub fn recompute_smoothness(
        &self,
        alg: &dyn SmoothingAlgorithm,
        horizon_lock: super::smoothing::horizon::HorizonLock,
        compute_params: &crate::ComputeParams,
    ) -> (TimeQuat, (f64, f64, f64)) {
        let file_metadata = self.file_metadata.read();
        let source_quaternions = self.video_query_range_quaternions_with_metadata(
            &self.quaternions,
            &file_metadata,
            compute_params,
        );
        let source_gravity_vectors = self.video_query_range_vectors_with_metadata(
            &file_metadata.gravity_vectors,
            &file_metadata,
            compute_params,
        );
        let source_duration_ms = compute_params.scaled_duration_ms;
        let mut smoothed_quaternions = source_quaternions.clone();

        for (ts, q) in smoothed_quaternions.iter_mut() {
            use crate::KeyframeType;
            let timestamp_ms = *ts as f64 / 1000.0;
            let additional_rotation_x = compute_params
                .keyframes
                .value_at_gyro_timestamp(&KeyframeType::AdditionalRotationX, timestamp_ms)
                .unwrap_or(compute_params.additional_rotation.0)
                * DEG2RAD;
            let additional_rotation_y = compute_params
                .keyframes
                .value_at_gyro_timestamp(&KeyframeType::AdditionalRotationY, timestamp_ms)
                .unwrap_or(compute_params.additional_rotation.1)
                * DEG2RAD;
            let additional_rotation_z = compute_params
                .keyframes
                .value_at_gyro_timestamp(&KeyframeType::AdditionalRotationZ, timestamp_ms)
                .unwrap_or(compute_params.additional_rotation.2)
                * DEG2RAD;
            let additional_rotation = Quat64::from_euler_angles(
                additional_rotation_y,
                additional_rotation_x,
                additional_rotation_z,
            );

            *q *= additional_rotation;
        }

        if true {
            // Lock horizon, then smooth
            horizon_lock.lock(
                &mut smoothed_quaternions,
                &source_quaternions,
                &source_gravity_vectors,
                self.use_gravity_vectors,
                self.integration_method,
                compute_params,
            );
            smoothed_quaternions =
                alg.smooth(&smoothed_quaternions, source_duration_ms, compute_params);
        } else {
            // Smooth, then lock horizon
            smoothed_quaternions =
                alg.smooth(&smoothed_quaternions, source_duration_ms, compute_params);
            horizon_lock.lock(
                &mut smoothed_quaternions,
                &source_quaternions,
                &source_gravity_vectors,
                self.use_gravity_vectors,
                self.integration_method,
                compute_params,
            );
        }

        let max_angles = crate::Smoothing::get_max_angles(
            &source_quaternions,
            &smoothed_quaternions,
            compute_params,
        );

        for (ts, sq) in smoothed_quaternions.iter_mut() {
            let org_quat =
                Self::clamped_quat_at_gyro_timestamp(&source_quaternions, *ts as f64 / 1000.0);
            // rotation quaternion from smooth motion -> raw motion to counteract it
            *sq = sq.inverse() * org_quat;
        }
        (smoothed_quaternions, max_angles)
    }

    pub fn raw_imu<'a>(&'a self, file_metadata: &'a FileMetadata) -> &'a Vec<TimeIMU> {
        if !self.raw_imu.is_empty() {
            return &self.raw_imu;
        }
        return &file_metadata.raw_imu;
    }

    pub fn set_offset(&mut self, timestamp_us: i64, offset_ms: f64) {
        if offset_ms.is_finite() && !offset_ms.is_nan() {
            match self.offsets.entry(timestamp_us) {
                Entry::Occupied(o) => {
                    *o.into_mut() = offset_ms;
                }
                Entry::Vacant(v) => {
                    v.insert(offset_ms);
                }
            }
            self.adjust_offsets();
        }
    }
    pub fn remove_offset(&mut self, timestamp_us: i64) {
        self.offsets.remove(&timestamp_us);
        self.adjust_offsets();
    }
    pub fn clear_offsets(&mut self) {
        self.offsets.clear();
        self.offsets_adjusted.clear();
    }
    pub fn get_offsets(&self) -> &BTreeMap<i64, f64> {
        &self.offsets
    }
    pub fn get_offsets_plus_linear(&self) -> BTreeMap<i64, (f64, f64)> {
        self.offsets
            .iter()
            .map(|(k, v)| (*k, (*v, self.offsets_linear.get(k).copied().unwrap_or(*v))))
            .collect()
    }
    pub fn set_offsets(&mut self, offsets: BTreeMap<i64, f64>) {
        self.offsets = offsets;
        self.adjust_offsets();
    }
    pub fn remove_offsets_near(&mut self, ts: i64, range_ms: f64) {
        let range_us = (range_ms * 1000.0).round() as i64;
        self.offsets
            .retain(|k, _| !(ts - range_us..ts + range_us).contains(k));
        self.adjust_offsets();
    }

    fn line_fit(offsets: &BTreeMap<i64, f64>) -> Option<[f64; 3]> {
        let a = OMatrix::<f64, nalgebra::Dyn, U2>::from_row_iterator(
            offsets.len(),
            offsets.iter().flat_map(|(k, _)| [*k as f64, 1.0]),
        );
        let b = OVector::<f64, nalgebra::Dyn>::from_iterator(
            offsets.len(),
            offsets.iter().map(|(_, v)| *v),
        );

        let svd = nalgebra::linalg::SVD::new(a.clone(), true, true);
        let solution = svd.solve(&b, 1e-14).ok()?;
        if solution.len() >= 2 {
            let model: OVector<f64, nalgebra::Dyn> = a * &solution;
            let l1: OVector<f64, nalgebra::Dyn> = model - b;
            let residuals: f64 = l1.dot(&l1);

            Some([solution[0], solution[1], residuals])
        } else {
            None
        }
    }

    pub fn adjust_offsets(&mut self) {
        if self.prevent_recompute {
            return;
        }
        // Calculate line fit
        if self.offsets.len() > 1 {
            let len = self.offsets.len();
            let keys: Vec<i64> = self.offsets.keys().copied().collect();

            #[derive(Default)]
            struct Params {
                offsets: BTreeMap<i64, f64>,
                rsquared: f64,
                coeffs: [f64; 3],
            }
            let mut best = Params {
                rsquared: 1000.0,
                ..Default::default()
            };

            let max_fitting_error = 5.0; // max 5 ms

            for i in 0..len {
                for j in 0..len {
                    if i != j {
                        let i_offset = self.offsets.get(&keys[i]).unwrap();
                        let j_offset = self.offsets.get(&keys[j]).unwrap();
                        let slope = (j_offset - i_offset) / (keys[j] - keys[i]) as f64;
                        let intersect = i_offset - keys[i] as f64 * slope;

                        let within_error: BTreeMap<i64, f64> = self
                            .offsets
                            .iter()
                            .filter_map(|(k, v)| {
                                if ((*k as f64 * slope + intersect) - *v).abs() < max_fitting_error
                                {
                                    Some((*k, *v))
                                } else {
                                    None
                                }
                            })
                            .collect();

                        if within_error.len() >= best.offsets.len() && within_error != best.offsets
                        {
                            if let Some(solution) = Self::line_fit(&within_error) {
                                let close_constant = solution[0].abs() < 0.1;
                                if within_error.len() > 2 && close_constant {
                                    if solution[2] < best.rsquared {
                                        best = Params {
                                            rsquared: solution[2],
                                            offsets: within_error.clone(),
                                            coeffs: solution.clone(),
                                        };
                                    }
                                } else if close_constant {
                                    best = Params {
                                        rsquared: best.rsquared,
                                        offsets: within_error.clone(),
                                        coeffs: solution.clone(),
                                    };
                                }
                            }
                        }
                    }
                }
            }

            self.offsets_linear.clear();
            if !best.offsets.is_empty() {
                for (k, _) in &self.offsets {
                    let fitted = *k as f64 * best.coeffs[0] + best.coeffs[1];
                    self.offsets_linear.insert(*k, fitted);
                }
            } else {
                if let Some(solution) = Self::line_fit(&self.offsets) {
                    for (k, _) in &self.offsets {
                        let fitted = *k as f64 * solution[0] + solution[1];
                        self.offsets_linear.insert(*k, fitted);
                    }
                }
            }
        } else {
            self.offsets_linear = self.offsets.clone();
        }

        self.offsets_adjusted = self
            .offsets
            .iter()
            .map(|(k, v)| (*k + (*v * 1000.0).round() as i64, *v))
            .collect::<BTreeMap<i64, f64>>();
    }

    pub fn apply_transforms(&mut self) {
        let file_metadata = self.file_metadata.read();

        if self.imu_transforms.has_any() {
            self.raw_imu = file_metadata.raw_imu.clone();
            for x in self.raw_imu.iter_mut() {
                if let Some(g) = x.gyro.as_mut() {
                    self.imu_transforms.transform(g, false);
                }
                if let Some(a) = x.accl.as_mut() {
                    self.imu_transforms.transform(a, true);
                }
                if let Some(m) = x.magn.as_mut() {
                    self.imu_transforms.transform(m, false);
                }
            }
            if self.imu_transforms.imu_lpf > 0.0
                && !file_metadata.raw_imu.is_empty()
                && self.duration_ms > 0.0
            {
                let sample_rate = file_metadata.raw_imu.len() as f64 / (self.duration_ms / 1000.0);
                if let Err(e) = super::filtering::Lowpass::filter_gyro_forward_backward(
                    self.imu_transforms.imu_lpf,
                    sample_rate,
                    &mut self.raw_imu,
                ) {
                    log::error!("Filter error {:?}", e);
                }
            }
            if self.imu_transforms.imu_mf > 0
                && !file_metadata.raw_imu.is_empty()
                && self.duration_ms > 0.0
            {
                let sample_rate = file_metadata.raw_imu.len() as f64 / (self.duration_ms / 1000.0);
                super::filtering::Median::filter_gyro_forward_backward(
                    self.imu_transforms.imu_mf,
                    sample_rate,
                    &mut self.raw_imu,
                );
            }
        } else {
            self.raw_imu.clear();
        }

        drop(file_metadata);

        self.integrate();
    }

    fn video_query_range_us(
        &self,
        file_metadata: &FileMetadata,
        compute_params: &crate::ComputeParams,
    ) -> Option<(i64, i64)> {
        if compute_params.frame_count == 0 || compute_params.scaled_fps <= 0.0 {
            return None;
        }

        let frame_readout_half = compute_params.frame_readout_time.abs() / 2.0;
        let mut video_times = Vec::with_capacity(compute_params.frame_count * 2);

        for frame in 0..compute_params.frame_count {
            let timestamp_ms =
                frame as f64 / compute_params.scaled_fps * 1000.0
                    + file_metadata
                        .per_frame_time_offsets
                        .get(frame)
                        .unwrap_or(&0.0);

            video_times.push(timestamp_ms - frame_readout_half);
            video_times.push(timestamp_ms + frame_readout_half);
        }

        let (min_video_ms, max_video_ms) =
            video_times
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, timestamp_ms| {
                    (acc.0.min(*timestamp_ms), acc.1.max(*timestamp_ms))
                });

        for ts in self.offsets_adjusted.keys() {
            let timestamp_ms = *ts as f64 / 1000.0;
            if timestamp_ms >= min_video_ms && timestamp_ms <= max_video_ms {
                video_times.push(timestamp_ms);
            }
        }

        let (min_gyro_us, max_gyro_us) =
            video_times
                .iter()
                .fold((i64::MAX, i64::MIN), |acc, timestamp_ms| {
                    let gyro_timestamp_ms =
                        timestamp_ms - self.offset_at_video_timestamp(*timestamp_ms);
                    let gyro_timestamp_us = (gyro_timestamp_ms * 1000.0).round() as i64;
                    (acc.0.min(gyro_timestamp_us), acc.1.max(gyro_timestamp_us))
                });

        Some((min_gyro_us, max_gyro_us))
    }

    fn trim_quats_to_gyro_range(quats: &TimeQuat, start_us: i64, end_us: i64) -> TimeQuat {
        if quats.is_empty() {
            return TimeQuat::new();
        }

        let first_ts = *quats.keys().next().unwrap();
        let last_ts = *quats.keys().next_back().unwrap();
        let start_us = start_us.max(first_ts).min(last_ts);
        let end_us = end_us.max(first_ts).min(last_ts);
        let (start_us, end_us) = if start_us <= end_us {
            (start_us, end_us)
        } else {
            (end_us, start_us)
        };

        let mut ret = TimeQuat::new();
        ret.insert(
            start_us,
            Self::clamped_quat_at_gyro_timestamp(quats, start_us as f64 / 1000.0),
        );
        ret.extend(quats.range(start_us..=end_us).map(|(&ts, &quat)| (ts, quat)));
        ret.insert(
            end_us,
            Self::clamped_quat_at_gyro_timestamp(quats, end_us as f64 / 1000.0),
        );
        ret
    }

    fn trim_vectors_to_gyro_range(vectors: &TimeVec, start_us: i64, end_us: i64) -> TimeVec {
        if vectors.is_empty() {
            return TimeVec::new();
        }

        let first_ts = *vectors.keys().next().unwrap();
        let last_ts = *vectors.keys().next_back().unwrap();
        let start_us = start_us.max(first_ts).min(last_ts);
        let end_us = end_us.max(first_ts).min(last_ts);
        let (start_us, end_us) = if start_us <= end_us {
            (start_us, end_us)
        } else {
            (end_us, start_us)
        };

        let mut ret = TimeVec::new();
        if let Some(vec) =
            super::smoothing::horizon::HorizonLock::interpolate_gravity_vector(vectors, start_us)
        {
            ret.insert(start_us, vec);
        }
        ret.extend(vectors.range(start_us..=end_us).map(|(&ts, &vec)| (ts, vec)));
        if let Some(vec) =
            super::smoothing::horizon::HorizonLock::interpolate_gravity_vector(vectors, end_us)
        {
            ret.insert(end_us, vec);
        }
        ret
    }

    fn video_query_range_quaternions_with_metadata(
        &self,
        quats: &TimeQuat,
        file_metadata: &FileMetadata,
        compute_params: &crate::ComputeParams,
    ) -> TimeQuat {
        if let Some((start_us, end_us)) =
            self.video_query_range_us(file_metadata, compute_params)
        {
            Self::trim_quats_to_gyro_range(quats, start_us, end_us)
        } else {
            quats.clone()
        }
    }

    #[cfg(test)]
    fn video_query_range_quaternions(
        &self,
        quats: &TimeQuat,
        compute_params: &crate::ComputeParams,
    ) -> TimeQuat {
        let file_metadata = self.file_metadata.read();
        self.video_query_range_quaternions_with_metadata(quats, &file_metadata, compute_params)
    }

    fn video_query_range_vectors_with_metadata(
        &self,
        vectors: &Option<TimeVec>,
        file_metadata: &FileMetadata,
        compute_params: &crate::ComputeParams,
    ) -> Option<TimeVec> {
        vectors.as_ref().map(|vectors| {
            if let Some((start_us, end_us)) =
                self.video_query_range_us(file_metadata, compute_params)
            {
                Self::trim_vectors_to_gyro_range(vectors, start_us, end_us)
            } else {
                vectors.clone()
            }
        })
    }

    pub fn clamped_quat_at_gyro_timestamp(quats: &TimeQuat, timestamp_ms: f64) -> Quat64 {
        match quats.len() {
            0 => return Quat64::identity(),
            1 => return *quats.values().next().unwrap(),
            _ => {}
        }

        if let Some(&first_ts) = quats.keys().next() {
            if let Some(&last_ts) = quats.keys().next_back() {
                let lookup_ts = ((timestamp_ms * 1000.0).round() as i64)
                    .min(last_ts)
                    .max(first_ts);

                if let Some(quat1) = quats.range(..=lookup_ts).next_back() {
                    if *quat1.0 == lookup_ts {
                        return *quat1.1;
                    }
                    if let Some(quat2) = quats.range(lookup_ts..).next() {
                        let time_delta = (quat2.0 - quat1.0) as f64;
                        let fract = (lookup_ts - quat1.0) as f64 / time_delta;
                        return quat1.1.slerp(quat2.1, fract);
                    }
                }
            }
        }
        Quat64::identity()
    }

    fn quat_at_timestamp(&self, quats: &TimeQuat, mut timestamp_ms: f64) -> Quat64 {
        if self.duration_ms <= 0.0 {
            return Quat64::identity();
        }

        timestamp_ms -= self.offset_at_video_timestamp(timestamp_ms);
        Self::clamped_quat_at_gyro_timestamp(quats, timestamp_ms)
    }

    pub fn org_quat_at_timestamp(&self, timestamp_ms: f64) -> Quat64 {
        self.quat_at_timestamp(&self.quaternions, timestamp_ms)
    }
    pub fn smoothed_quat_at_timestamp(&self, timestamp_ms: f64) -> Quat64 {
        self.quat_at_timestamp(&self.smoothed_quaternions, timestamp_ms)
    }

    pub fn offset_at_timestamp(offsets: &BTreeMap<i64, f64>, timestamp_ms: f64) -> f64 {
        match offsets.len() {
            0 => 0.0,
            1 => *offsets.values().next().unwrap(),
            _ => {
                if let Some(&first_ts) = offsets.keys().next() {
                    if let Some(&last_ts) = offsets.keys().next_back() {
                        let timestamp_us = (timestamp_ms * 1000.0) as i64;
                        let lookup_ts = (timestamp_us).min(last_ts - 1).max(first_ts + 1);
                        if let Some(offs1) = offsets.range(..=lookup_ts).next_back() {
                            if *offs1.0 == lookup_ts {
                                return *offs1.1;
                            }
                            if let Some(offs2) = offsets.range(lookup_ts..).next() {
                                let time_delta = (offs2.0 - offs1.0) as f64;
                                let fract = (timestamp_us - offs1.0) as f64 / time_delta;
                                return offs1.1 + (offs2.1 - offs1.1) * fract;
                            }
                        }
                    }
                }

                0.0
            }
        }
    }
    pub fn offset_at_video_timestamp(&self, timestamp_ms: f64) -> f64 {
        Self::offset_at_timestamp(&self.offsets_adjusted, timestamp_ms)
    }
    pub fn offset_at_gyro_timestamp(&self, timestamp_ms: f64) -> f64 {
        Self::offset_at_timestamp(&self.offsets, timestamp_ms)
    }

    pub fn get_checksum(&self) -> u64 {
        use std::hash::Hasher;
        let file_metadata = self.file_metadata.read();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(v) = &file_metadata.detected_source {
            hasher.write(v.as_bytes());
        }
        if let Some(v) = &self.imu_transforms.imu_orientation {
            hasher.write(v.as_bytes());
        }
        if let Some(v) = &self.imu_transforms.imu_rotation_angles {
            hasher.write_u64(v[0].to_bits());
            hasher.write_u64(v[1].to_bits());
            hasher.write_u64(v[2].to_bits());
        }
        if let Some(v) = &self.imu_transforms.acc_rotation_angles {
            hasher.write_u64(v[0].to_bits());
            hasher.write_u64(v[1].to_bits());
            hasher.write_u64(v[2].to_bits());
        }
        if let Some(v) = &self.imu_transforms.gyro_bias {
            hasher.write_u64(v[0].to_bits());
            hasher.write_u64(v[1].to_bits());
            hasher.write_u64(v[2].to_bits());
        }
        hasher.write(self.file_url.as_bytes());
        hasher.write_u64(self.duration_ms.to_bits());
        hasher.write_u64(self.imu_transforms.imu_lpf.to_bits());
        hasher.write_i32(self.imu_transforms.imu_mf);
        hasher.write_usize(self.raw_imu.len());
        hasher.write_usize(file_metadata.raw_imu.len());
        hasher.write_usize(self.quaternions.len());
        hasher.write_usize(file_metadata.quaternions.len());
        hasher.write_usize(
            file_metadata
                .image_orientations
                .as_ref()
                .map(|v| v.len())
                .unwrap_or_default(),
        );
        hasher.write_usize(file_metadata.lens_positions.len());
        hasher.write_usize(file_metadata.lens_params.len());
        hasher.write_u32(if self.use_gravity_vectors { 1 } else { 0 });
        hasher.write_usize(self.integration_method);
        for (ts, v) in &self.offsets {
            hasher.write_i64(*ts);
            hasher.write_u64(v.to_bits());
        }
        if let Some((ts, q)) = self.quaternions.first_key_value() {
            let v = q.as_vector();
            hasher.write_i64(*ts);
            hasher.write_u64(v[0].to_bits());
            hasher.write_u64(v[1].to_bits());
            hasher.write_u64(v[2].to_bits());
            hasher.write_u64(v[3].to_bits());
        }
        if let Some((ts, q)) = self.quaternions.last_key_value() {
            let v = q.as_vector();
            hasher.write_i64(*ts);
            hasher.write_u64(v[0].to_bits());
            hasher.write_u64(v[1].to_bits());
            hasher.write_u64(v[2].to_bits());
            hasher.write_u64(v[3].to_bits());
        }

        hasher.finish()
    }

    pub fn get_sample_rate(file_metadata: &FileMetadata) -> f64 {
        if file_metadata.raw_imu.len() > 2 {
            let len = file_metadata.raw_imu.len() as f64;
            let duration_ms = file_metadata.raw_imu.last().unwrap().timestamp_ms
                - file_metadata.raw_imu.first().unwrap().timestamp_ms;
            let duration_ms = duration_ms * ((len + 1.0) / len.max(1.0));
            file_metadata.raw_imu.len() as f64 / (duration_ms / 1000.0)
        } else if file_metadata.quaternions.len() > 2 {
            let len = file_metadata.quaternions.len() as f64;
            let first = *file_metadata.quaternions.iter().next().unwrap().0 as f64 / 1000.0;
            let last = *file_metadata.quaternions.iter().next_back().unwrap().0 as f64 / 1000.0;
            let duration_ms = last - first;
            let duration_ms = duration_ms * ((len + 1.0) / len.max(1.0));
            file_metadata.quaternions.len() as f64 / (duration_ms / 1000.0)
        } else {
            0.0
        }
    }

    pub fn find_bias(&self, timestamp_start: f64, timestamp_stop: f64) -> (f64, f64, f64) {
        let ts_start = timestamp_start - self.offset_at_video_timestamp(timestamp_start);
        let ts_stop = timestamp_stop - self.offset_at_video_timestamp(timestamp_stop);
        let mut bias_vals = [0.0, 0.0, 0.0];
        let mut n = 0;

        let file_metadata = self.file_metadata.read();

        for x in &file_metadata.raw_imu {
            if let Some(g) = x.gyro {
                if x.timestamp_ms > ts_start && x.timestamp_ms < ts_stop {
                    bias_vals[0] -= g[0];
                    bias_vals[1] -= g[1];
                    bias_vals[2] -= g[2];
                    n += 1;
                }
            }
        }
        for b in bias_vals.iter_mut() {
            *b /= n.max(1) as f64;
        }

        (bias_vals[0], bias_vals[1], bias_vals[2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_quat_json(quat: UnitQuaternion<f64>) -> serde_json::Value {
        let q = quat.quaternion();
        serde_json::json!({
            "init quart": {
                "t": 0,
                "v": {
                    "w": q.w,
                    "x": q.i,
                    "y": q.j,
                    "z": q.k
                }
            }
        })
    }

    fn imu_sample(timestamp_ms: f64, accl: [f64; 3]) -> TimeIMU {
        TimeIMU {
            timestamp_ms,
            gyro: Some([0.0, 0.0, 0.0]),
            accl: Some(accl),
            magn: None,
        }
    }

    #[test]
    fn compute_auto_rotation_uses_init_quaternion() {
        let cases = [
            (UnitQuaternion::identity(), 0),
            (
                UnitQuaternion::from_euler_angles(-std::f64::consts::FRAC_PI_2, 0.0, 0.0),
                90,
            ),
            (
                UnitQuaternion::from_euler_angles(0.0, std::f64::consts::FRAC_PI_2, 0.0),
                180,
            ),
            (
                UnitQuaternion::from_euler_angles(std::f64::consts::FRAC_PI_2, 0.0, 0.0),
                270,
            ),
        ];

        for (quat, expected) in cases {
            let additional_data = init_quat_json(quat);
            assert_eq!(
                compute_auto_rotation(Some(&additional_data), &[], 0.0, true),
                Some(expected)
            );
        }
    }

    #[test]
    fn compute_auto_rotation_without_init_quaternion_returns_none() {
        let raw_imu = vec![imu_sample(0.0, [0.0, 0.0, 9.80665])];
        assert_eq!(compute_auto_rotation(None, &raw_imu, 1.0, false), None);
    }

    #[test]
    fn load_from_telemetry_duration_mismatch_warning_removed() {
        let source = include_str!("mod.rs");
        let needle = concat!(
            "IMU duration",
            " {imu_duration}",
            " is different",
            " than video duration"
        );

        assert!(!source.contains(needle));
    }

    #[test]
    fn debug_auto_rotation_from_bin_file() {
        // 测试1: 用分段 BIN 文件（每个文件独立处理）
        let test_dir =
            "D:/Gyroflow_NiYien/Test_function/AutoRotate/2026-04-09/Temp/2026-04-09_11-38-31_mix";
        let test_path = std::path::Path::new(test_dir);
        if !test_path.exists() {
            eprintln!("Test dir not found, skipping");
            return;
        }

        let expected = [0, 0, 90, 90, 270, 270, 180, 180, 0, 0, 0];

        fn load_bin(file_path: &std::path::Path) -> (Vec<TimeIMU>, serde_json::Value) {
            let mut stream = std::fs::File::open(file_path).unwrap();
            let filesize = stream.metadata().unwrap().len() as usize;
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let options = telemetry_parser::InputOptions::default();
            let input = telemetry_parser::Input::from_stream_with_options(
                &mut stream,
                filesize,
                file_path,
                |_| {},
                cancel,
                options,
            )
            .unwrap();
            let raw_imu =
                telemetry_parser::util::normalized_imu_interpolated(&input, Some("XYZ".into()))
                    .unwrap();
            let mut additional_data = serde_json::Value::Object(serde_json::Map::new());
            if let Some(ref samples) = input.samples {
                for info in samples {
                    if let Some(ref tag_map) = info.tag_map {
                        if let Some(map) =
                            tag_map.get(&telemetry_parser::tags_impl::GroupId::Default)
                        {
                            if let Some(v) = map
                                .get(&telemetry_parser::tags_impl::TagId::Metadata)
                                .map(|t| &t.value)
                            {
                                if let telemetry_parser::tags_impl::TagValue::Json(v) = v {
                                    crate::util::merge_json(&mut additional_data, v.get());
                                }
                            }
                        }
                    }
                }
            }
            (raw_imu, additional_data)
        }

        // 测试1: 分段文件
        eprintln!("\n=== Test 1: Individual split BIN files ===");
        let mut files: Vec<_> = std::fs::read_dir(test_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |e| e == "bin"))
            .collect();
        files.sort();
        let mut state = SenseFlowAutoRotationState::default();
        for (i, file_path) in files.iter().enumerate() {
            let fname = file_path.file_name().unwrap().to_string_lossy().to_string();
            let (raw_imu, additional_data) = load_bin(file_path);
            let result = compute_auto_rotation_for_segment_with_state(
                &mut state,
                &raw_imu,
                Some(&additional_data),
                &fname,
            );
            let exp = expected.get(i).copied().unwrap_or(-1);
            let got = result.unwrap_or(-1);
            let status = if got == exp { "OK" } else { "FAIL" };
            eprintln!(
                "[split] {}: expected={} got={} (imu_len={})",
                fname,
                exp,
                got,
                raw_imu.len()
            );
        }

        // 测试2: 原始 MIX 文件，按时间范围切片（模拟 batch auto match）
        eprintln!("\n=== Test 2: Original MIX file with time-range slicing ===");
        let mix_path = std::path::Path::new(
            "D:/Gyroflow_NiYien/Test_function/AutoRotate/2026-04-09/2026-04-09_11-38-31_mix.bin",
        );
        if !mix_path.exists() {
            eprintln!("MIX file not found");
            return;
        }

        // 读取第一个 split 文件的 first sample，然后在 MIX 的 raw_imu 中找到匹配时间
        let (mix_imu, mix_additional) = load_bin(mix_path);
        eprintln!(
            "MIX file: total_imu={} first_ts={:.1} last_ts={:.1}",
            mix_imu.len(),
            mix_imu.first().map(|s| s.timestamp_ms).unwrap_or(0.0),
            mix_imu.last().map(|s| s.timestamp_ms).unwrap_or(0.0),
        );

        // 对每个 split 文件，找到其在 MIX 中的起始时间
        let mut state2 = SenseFlowAutoRotationState::default();
        for (i, file_path) in files.iter().enumerate() {
            let fname = file_path.file_name().unwrap().to_string_lossy().to_string();
            let (seg_imu, _) = load_bin(file_path);
            if seg_imu.is_empty() {
                continue;
            }

            // 用 first sample 的 gyro/accl 值在 MIX 中查找匹配位置
            let first_seg = &seg_imu[0];
            let mut found_idx = None;
            for (j, mix_s) in mix_imu.iter().enumerate() {
                if let (Some(sg), Some(mg)) = (first_seg.gyro, mix_s.gyro) {
                    if (sg[0] - mg[0]).abs() < 1e-6
                        && (sg[1] - mg[1]).abs() < 1e-6
                        && (sg[2] - mg[2]).abs() < 1e-6
                    {
                        found_idx = Some(j);
                        break;
                    }
                }
            }

            let Some(idx) = found_idx else {
                eprintln!("[mix] {}: could not find matching sample in MIX!", fname);
                continue;
            };

            let start_ms = mix_imu[idx].timestamp_ms;
            let end_ms = mix_imu
                .last()
                .map(|s| s.timestamp_ms)
                .unwrap_or(start_ms + 100000.0);

            // 模拟 clone_metadata_for_job 的时间切片
            let sliced: Vec<TimeIMU> = mix_imu
                .iter()
                .filter(|s| s.timestamp_ms >= start_ms && s.timestamp_ms <= end_ms)
                .cloned()
                .collect();

            let result = compute_auto_rotation_for_segment_with_state(
                &mut state2,
                &sliced,
                Some(&mix_additional),
                &fname,
            );
            let exp = expected.get(i).copied().unwrap_or(-1);
            let got = result.unwrap_or(-1);
            let status = if got == exp { "OK" } else { "FAIL" };
            eprintln!(
                "[mix] {}: expected={} got={} (mix_idx={} start_ms={:.1} sliced_len={})",
                fname,
                exp,
                got,
                idx,
                start_ms,
                sliced.len()
            );
        }
    }

    #[test]
    fn niyien_tool_sample_init_quaternion_maps_to_zero() {
        let additional_data = serde_json::json!({
            "init quart": {
                "t": 0,
                "v": {
                    "w": 0.3280279338359833,
                    "x": -0.6184462308883667,
                    "y": -0.3420819640159607,
                    "z": -0.6268119812011719
                }
            }
        });
        assert_eq!(
            compute_auto_rotation(Some(&additional_data), &[], 0.0, true),
            Some(0)
        );
    }

    #[test]
    fn smoothing_source_quaternions_are_limited_to_video_query_range_with_offset() {
        let mut gyro = GyroSource::new();
        gyro.duration_ms = 12_000.0;
        gyro.quaternions = (0..=12)
            .map(|second| {
                (
                    second * 1_000_000,
                    Quat64::from_euler_angles(0.0, second as f64 * 0.01, 0.0),
                )
            })
            .collect();
        gyro.set_offsets(BTreeMap::from([
            (2_000_000, -2_000.0),
            (11_000_000, -2_000.0),
        ]));

        let compute_params = crate::ComputeParams {
            frame_count: 10,
            scaled_fps: 1.0,
            scaled_duration_ms: 10_000.0,
            ..Default::default()
        };
        let source_quaternions =
            gyro.video_query_range_quaternions(&gyro.quaternions, &compute_params);

        assert_eq!(
            source_quaternions.keys().copied().collect::<Vec<_>>(),
            (2..=11).map(|second| second * 1_000_000).collect::<Vec<_>>()
        );
    }

    #[test]
    fn smoothing_source_quaternions_include_per_frame_offsets_readout_and_offset_breakpoints() {
        let mut gyro = GyroSource::new();
        gyro.duration_ms = 12_000.0;
        gyro.quaternions = (0..=12)
            .map(|second| {
                (
                    second * 1_000_000,
                    Quat64::from_euler_angles(0.0, second as f64 * 0.01, 0.0),
                )
            })
            .collect();
        gyro.file_metadata.write().per_frame_time_offsets = vec![500.0, 500.0];
        gyro.set_offsets(BTreeMap::from([
            (0, 0.0),
            (2_000_000, -1_000.0),
            (3_000_000, -1_000.0),
        ]));

        let compute_params = crate::ComputeParams {
            frame_count: 2,
            scaled_fps: 1.0,
            scaled_duration_ms: 2_000.0,
            frame_readout_time: 200.0,
            ..Default::default()
        };
        let source_quaternions =
            gyro.video_query_range_quaternions(&gyro.quaternions, &compute_params);

        assert_eq!(
            source_quaternions.keys().copied().collect::<Vec<_>>(),
            vec![800_000, 1_000_000, 2_000_000, 2_600_000]
        );
    }

    #[derive(Clone, Default)]
    struct RecordingSmoothingAlgorithm {
        duration_ms: Arc<parking_lot::Mutex<Option<f64>>>,
        timestamps: Arc<parking_lot::Mutex<Vec<i64>>>,
    }

    impl SmoothingAlgorithm for RecordingSmoothingAlgorithm {
        fn get_name(&self) -> String {
            "Recording".to_owned()
        }

        fn get_parameters_json(&self) -> serde_json::Value {
            serde_json::json!([])
        }

        fn get_status_json(&self) -> serde_json::Value {
            serde_json::json!([])
        }

        fn set_parameter(&mut self, _name: &str, _val: f64) {}

        fn get_parameter(&self, _name: &str) -> f64 {
            0.0
        }

        fn get_checksum(&self) -> u64 {
            0
        }

        fn smooth(
            &self,
            quats: &TimeQuat,
            duration: f64,
            _compute_params: &crate::ComputeParams,
        ) -> TimeQuat {
            *self.duration_ms.lock() = Some(duration);
            *self.timestamps.lock() = quats.keys().copied().collect();
            quats.clone()
        }
    }

    #[test]
    fn recompute_smoothness_passes_only_video_query_range_to_smoothing_algorithm() {
        let mut gyro = GyroSource::new();
        gyro.duration_ms = 12_000.0;
        gyro.quaternions = (0..=12)
            .map(|second| {
                (
                    second * 1_000_000,
                    Quat64::from_euler_angles(0.0, second as f64 * 0.01, 0.0),
                )
            })
            .collect();

        let compute_params = crate::ComputeParams {
            frame_count: 10,
            scaled_fps: 1.0,
            scaled_duration_ms: 10_000.0,
            ..Default::default()
        };
        let alg = RecordingSmoothingAlgorithm::default();

        gyro.recompute_smoothness(
            &alg,
            crate::smoothing::horizon::HorizonLock::default(),
            &compute_params,
        );

        assert_eq!(
            alg.timestamps.lock().clone(),
            (0..=9).map(|second| second * 1_000_000).collect::<Vec<_>>()
        );
        assert_eq!(*alg.duration_ms.lock(), Some(10_000.0));
    }

    #[test]
    fn sony_readout_scale_keeps_original_when_sample_rates_are_invalid() {
        assert_eq!(
            scale_sony_frame_readout_time(Some(40.0), 0.0, 0.0),
            Some(40.0)
        );
        assert_eq!(
            scale_sony_frame_readout_time(Some(40.0), f64::NAN, 200.0),
            Some(40.0)
        );
    }

    #[test]
    fn sony_readout_scale_scales_when_sample_rates_are_valid() {
        assert_eq!(
            scale_sony_frame_readout_time(Some(40.0), 1000.0, 500.0),
            Some(20.0)
        );
    }

    // §6 GyroSource::integrate two-layer guard tests.

    fn nonempty_raw_imu_blob() -> Vec<TimeIMU> {
        (0..16)
            .map(|i| TimeIMU {
                timestamp_ms: i as f64 * 10.0,
                gyro: Some([0.1, 0.2, 0.3]),
                accl: Some([0.0, 0.0, 9.81]),
                magn: None,
            })
            .collect()
    }

    fn sentinel_quaternions() -> TimeQuat {
        let mut q = TimeQuat::new();
        q.insert(0, Quat64::from_euler_angles(0.1, 0.2, 0.3));
        q.insert(1_000_000, Quat64::from_euler_angles(0.4, 0.5, 0.6));
        q
    }

    #[test]
    fn integrate_with_zero_duration_preserves_quaternions() {
        let mut gs = GyroSource::new();
        let mut md = FileMetadata::default();
        md.raw_imu = nonempty_raw_imu_blob();
        *gs.file_metadata.write() = md;
        gs.integration_method = 2; // VQF
        gs.duration_ms = 0.0;
        let baseline = sentinel_quaternions();
        gs.quaternions = baseline.clone();

        gs.integrate();

        assert_eq!(gs.quaternions, baseline);
    }

    #[test]
    fn integrate_with_nan_duration_preserves_quaternions() {
        let mut gs = GyroSource::new();
        let mut md = FileMetadata::default();
        md.raw_imu = nonempty_raw_imu_blob();
        *gs.file_metadata.write() = md;
        gs.integration_method = 2;
        gs.duration_ms = f64::NAN;
        let baseline = sentinel_quaternions();
        gs.quaternions = baseline.clone();

        gs.integrate();

        assert_eq!(gs.quaternions, baseline);
    }

    #[test]
    fn integrate_preserves_quaternions_when_integrator_returns_empty() {
        // raw_imu is empty so VQFIntegrator returns empty (existing
        // is_empty guard), but duration_ms is valid so the outer entry
        // guard doesn't fire. The match arm conditional MUST preserve
        // the pre-existing quaternions.
        let mut gs = GyroSource::new();
        *gs.file_metadata.write() = FileMetadata::default();
        gs.integration_method = 2;
        gs.duration_ms = 1000.0;
        let baseline = sentinel_quaternions();
        gs.quaternions = baseline.clone();

        gs.integrate();

        assert_eq!(gs.quaternions, baseline);
    }
}
