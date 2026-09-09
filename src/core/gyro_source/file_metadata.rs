// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::{TimeIMU, TimeQuat, TimeVec, splines};
use crate::camera_identifier::CameraIdentifier;
use crate::stabilization_params::ReadoutDirection;

#[cfg(test)]
mod sony_project_compat_tests {
    use super::*;

    #[test]
    fn scalar_and_dual_axis_focal_lengths_round_trip() {
        let old: LensParams = serde_json::from_str(r#"{"pixel_focal_length":1200.0}"#).unwrap();
        assert_eq!(old.pixel_focal_length, Some((1200.0, 1200.0)));
        let current = LensParams {
            pixel_focal_length: Some((1200.0, 1300.0)),
            principal_point: Some((950.0, 545.0)),
            ..Default::default()
        };
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&current, &mut bytes).unwrap();
        let restored: LensParams = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(restored.pixel_focal_length, current.pixel_focal_length);
        assert_eq!(restored.principal_point, current.principal_point);
    }

    #[test]
    fn old_focal_plane_only_mesh_keeps_crop_and_clears_legacy_storage() {
        let mut forward = vec![9.0, 0.0, 0.0, 6000.0, 4000.0, 100.0, 200.0, 3840.0, 2160.0];
        forward.extend([8.0, 0.0, 500.0, 1.0]);
        forward.extend([0.001; 16]);
        let inverse = forward.iter().map(|v| *v as f32).collect();
        let mut md = FileMetadata {
            legacy_mesh_correction: vec![(forward, inverse)],
            keep_video_gyro: true,
            ..Default::default()
        };
        super::super::sony::upgrade_legacy_mesh_buffers(&mut md);
        assert!(md.legacy_mesh_correction.is_empty());
        assert!(md.mesh_correction.has_focal_plane(0));
        assert!(!md.mesh_correction.has_mesh(0));
        let packed = md.mesh_correction.kernel_buffer(0);
        assert_eq!(&packed[5..9], &[100.0, 200.0, 3840.0, 2160.0]);
        assert_eq!(packed[9], 8.0);
        assert_eq!(packed.last(), Some(&0.0));
        assert!(md.thin().keep_video_gyro);
        assert!(md.thin().mesh_correction.is_empty());
    }
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LensParams {
    pub focal_length: Option<f32>,               // millimeters
    pub pixel_pitch: Option<(u32, u32)>,         // nanometers
    pub sensor_size_px: Option<(u32, u32)>,      // pixels
    pub capture_area_origin: Option<(f32, f32)>, // pixels
    pub capture_area_size: Option<(f32, f32)>,   // pixels
    #[serde(deserialize_with = "deserialize_pixel_focal_length")]
    pub pixel_focal_length: Option<(f32, f32)>, // (fx, fy) pixels
    pub principal_point: Option<(f32, f32)>, // (cx, cy) pixels
    pub distortion_coefficients: Vec<f64>,
    pub focus_distance: Option<f32>,     // meters
    pub iris_fstop: Option<f32>,         // f-number
    pub iris_tstop: Option<f32>,         // T-number
    pub zoom_ring_position: Option<f32>, // percent of the zoom ring travel
}
impl LensParams {
    /// Whether the imager geometry is complete enough to be used for the stabilization
    pub fn has_geometry(&self) -> bool {
        self.pixel_pitch.is_some()
            && self.capture_area_size.is_some()
            && (self.pixel_focal_length.is_some() || self.focal_length.is_some())
    }
    pub fn has_descriptive_data(&self) -> bool {
        self.focus_distance.is_some() || self.iris_fstop.is_some() || self.iris_tstop.is_some()
    }
    pub fn has_projection_data(&self) -> bool {
        self.pixel_focal_length.is_some()
            || (self.focal_length.is_some()
                && self.pixel_pitch.is_some()
                && self.capture_area_size.is_some())
            || !self.distortion_coefficients.is_empty()
    }
    pub fn has_readout_scale(&self) -> bool {
        self.capture_area_size.is_some() && self.sensor_size_px.is_some()
    }
}

fn deserialize_pixel_focal_length<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<(f32, f32)>, D::Error> {
    use serde::Deserialize;
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Compat { Pair((f32, f32)), Single(f32) }
    Ok(Option::<Compat>::deserialize(d)?.map(|v| match v {
        Compat::Pair(p) => p,
        Compat::Single(f) => (f, f),
    }))
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CameraStabData {
    pub offset: f64,
    pub ois_offset: Option<f64>,
    pub sensor_size: (u32, u32),
    pub crop_area: (f32, f32, f32, f32),
    pub pixel_pitch: (u32, u32),
    pub ibis_spline: splines::CatmullRom<nalgebra::Vector3<f64>>,
    pub ois_spline: splines::CatmullRom<nalgebra::Vector3<f64>>,
}

// Lens breathing compensation of one frame: output zoom per band of the capture area rows, linearly interpolated
// between the bands, as many as the frame's focus motion during the readout needs (a single entry when the rows
// agree or the frame has no rolling-shutter table), see gyro_source::sony::breathing
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BreathingFrame {
    pub scale: Vec<f32>,
    pub crop_y: f32,
    pub crop_h: f32,
}
impl BreathingFrame {
    pub fn scale_at_row(&self, sensor_row: f64) -> f64 {
        let n = self.scale.len();
        if n < 2 {
            return self.scale.first().copied().unwrap_or(1.0) as f64;
        }
        let x = ((sensor_row - self.crop_y as f64) * (n - 1) as f64
            / (self.crop_h as f64 - 1.0).max(1.0))
        .clamp(0.0, (n - 1) as f64);
        let i = (x as usize).min(n - 2);
        let t = x - i as f64;
        self.scale[i] as f64 * (1.0 - t) + self.scale[i + 1] as f64 * t
    }
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FileMetadata {
    pub imu_orientation: Option<String>,
    pub raw_imu: Vec<TimeIMU>,
    pub quaternions: TimeQuat,
    pub gravity_vectors: Option<TimeVec>,
    pub image_orientations: Option<TimeQuat>,
    pub detected_source: Option<String>,
    /// True when telemetry-parser identifies the body as a RED Komodo / Komodo-X.
    /// Used by external IMU arbitration: a Komodo main video keeps its own gyro
    /// and rejects subsequent external IMU loads (see lib.rs::load_gyro_data).
    pub is_komodo: bool,
    /// True when the video's own built-in gyro is the trusted motion source and
    /// must be kept instead of being overwritten by an external IMU: RED Komodo
    /// (see `is_komodo`) or a Sony body that embedded gyro/quaternion samples.
    /// Generalizes the Komodo arbitration to Sony; checked by external IMU
    /// arbitration (lib.rs::load_gyro_data) and render_queue apply/auto-sync
    /// gating. Computed once at parse and propagated through `thin()` because
    /// `thin()` strips raw_imu/quaternions, which would make a later
    /// `has_motion()` check read false.
    pub keep_video_gyro: bool,
    pub frame_readout_time: Option<f64>,
    pub frame_readout_direction: ReadoutDirection,
    pub frame_rate: Option<f64>,
    pub record_frame_rate: Option<f64>,
    pub camera_identifier: Option<CameraIdentifier>,
    pub lens_profile: Option<serde_json::Value>,
    /// Canon synthetic opencv_standard lens, computed at parse time but held off
    /// plain load. Activated only after a batch senseflow match assigns external
    /// gyro to the job (see render_queue apply_match). Lets single-video loads
    /// stay bare while the batch flow reproduces the pre-change behaviour.
    /// See canon::build_synthetic_canon_lens_profile.
    pub canon_auto_lens_profile: Option<serde_json::Value>,
    pub lens_positions: BTreeMap<i64, f64>,
    pub lens_params: BTreeMap<i64, LensParams>,
    pub unit_pixel_focal_length: Option<f64>,
    pub digital_zoom: Option<f64>,
    pub has_accurate_timestamps: bool,
    pub creation_date: Option<String>,
    pub timezone_offset: Option<String>,
    pub creation_date_utc: Option<String>,
    /// SMPTE timecode as "HH:MM:SS:FF", currently only from CinemaDNG's 0xC763.
    /// A time of day with NO DATE and NO TIMEZONE, which is why it is not folded
    /// into the creation-date fields: BMD CinemaDNG writes this and nothing else
    /// time-related, so `creation_date*` stay None for it.
    pub timecode: Option<String>,
    pub additional_data: serde_json::Value,
    pub per_frame_time_offsets: Vec<f64>,
    /// Canon intrinsic frame-time series held inactive when the video carries
    /// trusted built-in gyro data. A batch assignment with external gyro metadata
    /// copies it into `per_frame_time_offsets`.
    pub canon_deferred_frame_time_offsets: Vec<f64>,
    pub camera_stab_data: Vec<CameraStabData>,
    #[serde(rename = "mesh_corrections")]
    pub mesh_correction: MeshCorrections,
    /// What older project files stored instead: one `(forward, inverse)` buffer pair per frame. Read only, and folded
    /// into `mesh_correction` on load (`sony::upgrade_legacy_mesh_buffers`)
    #[serde(rename = "mesh_correction", skip_serializing)]
    pub legacy_mesh_correction: Vec<(Vec<f64>, Vec<f32>)>,
    pub lens_breathing: Vec<BreathingFrame>,
    /// Cache of `lens_focal_length_varies`, the renderer asks per frame
    #[serde(skip)]
    pub focal_length_varies_cache: std::sync::OnceLock<bool>,
    pub duration_ms: f64,
}
impl FileMetadata {
    pub fn thin(&self) -> Self {
        Self {
            imu_orientation: self.imu_orientation.clone(),
            raw_imu: Default::default(),
            quaternions: Default::default(),
            gravity_vectors: Default::default(),
            image_orientations: Default::default(),
            detected_source: self.detected_source.clone(),
            is_komodo: self.is_komodo,
            keep_video_gyro: self.keep_video_gyro,
            frame_readout_time: self.frame_readout_time.clone(),
            frame_readout_direction: self.frame_readout_direction.clone(),
            frame_rate: self.frame_rate.clone(),
            record_frame_rate: self.record_frame_rate.clone(),
            camera_identifier: self.camera_identifier.clone(),
            lens_profile: self.lens_profile.clone(),
            canon_auto_lens_profile: self.canon_auto_lens_profile.clone(),
            lens_positions: Default::default(),
            lens_params: Default::default(),
            unit_pixel_focal_length: self.unit_pixel_focal_length.clone(),
            digital_zoom: self.digital_zoom.clone(),
            has_accurate_timestamps: self.has_accurate_timestamps.clone(),
            creation_date: self.creation_date.clone(),
            timezone_offset: self.timezone_offset.clone(),
            creation_date_utc: self.creation_date_utc.clone(),
            timecode: self.timecode.clone(),
            additional_data: self.additional_data.clone(),
            per_frame_time_offsets: Default::default(),
            canon_deferred_frame_time_offsets: Default::default(),
            camera_stab_data: Default::default(),
            mesh_correction: Default::default(),
            duration_ms: self.duration_ms,
            legacy_mesh_correction: Default::default(),
            lens_breathing: Default::default(),
            focal_length_varies_cache: Default::default(),
        }
    }
    pub fn has_motion(&self) -> bool {
        !self.raw_imu.is_empty() || !self.quaternions.is_empty()
    }
    /// Number of `lens_params` samples that feed the projection. The map also holds entries with
    /// nothing but the descriptive values, for cameras that report those but no geometry at all
    pub fn lens_geometry_count(&self) -> usize {
        self.lens_params
            .values()
            .filter(|x| x.has_projection_data())
            .count()
    }
    /// Whether any frame has a mesh or focal plane correction
    pub fn has_mesh_correction(&self) -> bool {
        !self.mesh_correction.is_empty()
    }
    /// More than 0.2% between the smallest and the largest value
    fn varies(it: &mut dyn Iterator<Item = f64>) -> bool {
        let (mut min, mut max, mut count) = (f64::MAX, 0.0f64, 0usize);
        for f in it.filter(|f| f.is_finite() && *f > 0.0) {
            min = min.min(f);
            max = max.max(f);
            count += 1;
        }
        count > 1 && max > min * 1.002
    }
    /// Whether the lens metadata reports a focal length in millimetres that changes over the clip (a zoom lens
    /// on a body that records it: Blackmagic, RED, Nikon, Z CAM, Sony). Cached, the projection asks per frame,
    /// see `FrameTransform::get_lens_data_at_timestamp`
    pub fn lens_focal_length_varies(&self) -> bool {
        *self.focal_length_varies_cache.get_or_init(|| {
            Self::varies(
                &mut self
                    .lens_params
                    .values()
                    .filter_map(|x| x.focal_length.map(|f| f as f64)),
            )
        })
    }
    /// Whether the lens metadata describes a focal length that changes over the clip (zoom lens, dynamic
    /// crop, interpolated lens profiles). A fixed lens reports the same value every frame and has nothing
    /// to stabilize or to chart. The pixel and the millimetre values are compared separately: an entry may
    /// carry one without the other, and they're not in the same unit
    pub fn has_per_frame_focal_length(&self) -> bool {
        Self::varies(&mut self.lens_params.values().filter_map(|x| {
            x.pixel_focal_length
                .map(|(fx, fy)| (fx as f64 * fy as f64).sqrt())
        })) || self.lens_focal_length_varies()
            || Self::varies(&mut self.lens_positions.values().copied())
    }
    pub fn lens_params_closest(
        &self,
        timestamp_us: i64,
        max_diff: i64,
        pred: impl Fn(&LensParams) -> bool,
    ) -> Option<&LensParams> {
        let max_diff = max_diff.max(0);
        let min_ts = timestamp_us.saturating_sub(max_diff);
        let max_ts = timestamp_us.saturating_add(max_diff);

        // The two ranges overlap on an exact key hit; the tie-break below then picks that same entry
        let before = self
            .lens_params
            .range(min_ts..=timestamp_us)
            .rev()
            .find(|(_, v)| pred(*v));
        let after = self
            .lens_params
            .range(timestamp_us..=max_ts)
            .find(|(_, v)| pred(*v));

        // `abs_diff` returns an u64 and can't overflow, unlike `(key - other).abs()`
        let closest = match (before, after) {
            (Some(before), Some(after)) => {
                if timestamp_us.abs_diff(*after.0) <= timestamp_us.abs_diff(*before.0) {
                    after
                } else {
                    before
                }
            }
            (Some(before), None) => before,
            (None, Some(after)) => after,
            (None, None) => return None,
        };
        // The ranges above are inclusive on both ends, so an entry exactly `max_diff` away still needs rejecting.
        // Anything farther than the closest matching entry is out of range as well, so one check is enough
        if timestamp_us.abs_diff(*closest.0) < max_diff as u64 {
            Some(closest.1)
        } else {
            None
        }
    }
}

// ------------- Mesh correction -------------

/// Length of the header every mesh block starts with: `[block length, divisions x, y, mesh size x, y, capture area
/// origin x, y, size x, y]`, followed by the grid positions (x, y per node, row by row) and the row spline coefficients
/// (`splines::BivariateSpline`, a, b, c, d per row, for x then for y). The kernels read `[0]` as the offset of the
/// focal plane table that follows the block, and a block longer than the header as a mesh
pub const MESH_HEADER: usize = 9;

/// Sony's per-frame distortion correction, from the `MeshCorrection` and `FocalPlaneDistortion` metadata (built by
/// `gyro_source::sony::get_mesh_correction`). The mesh depends on the lens state alone, so every frame the camera wrote
/// the same one for shares a single table; what differs from frame to frame, the capture area (which moves with the
/// in-camera stabilization) and the focal plane table, is kept per frame and folded into the buffers on request.
/// Project files with embedded metadata store the same thing, tables once and a few values per frame
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MeshCorrections {
    pub tables: Vec<MeshTable>,
    /// One per frame, `MeshFrame::is_empty` where the frame has none; the whole vector is empty when no frame has any
    pub frames: Vec<MeshFrame>,
}

/// One distortion mesh, in the block layout described at [`MESH_HEADER`]. The capture area slots of the headers hold
/// zeros, the frame supplies them
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MeshTable {
    /// The camera's mesh, what the CPU point path (`undistort_points`) applies
    pub forward: Vec<f64>,
    /// Its numeric inverse on the same grid, what the kernels look up
    pub inverse: Vec<f32>,
    /// `forward` for the kernels, which refine every inverse lookup against it; empty when the inverse alone is
    /// accurate enough (`sony::MESH_REFINE_THRESHOLD_PX`), which spares two spline evaluations per pixel
    pub refinement: Vec<f32>,
}

/// The correction of one frame
#[derive(Default, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MeshFrame {
    /// Index into `MeshCorrections::tables`, `None` when the frame has no mesh
    pub table: Option<u32>,
    /// Extent of the mesh coordinates (the sensor), in sensor pixels
    pub mesh_size: (f64, f64),
    /// Capture area within the sensor, in sensor pixels: the video maps onto it
    pub crop_origin: (f64, f64),
    pub crop_size: (f64, f64),
    /// Focal plane distortion table `[band count, unk1, band height, scale, count × (x, y)]`, empty when the frame has none
    pub focal_plane: Vec<f64>,
}
impl MeshFrame {
    pub fn is_empty(&self) -> bool {
        self.table.is_none() && !self.has_focal_plane()
    }
    pub fn has_focal_plane(&self) -> bool {
        self.focal_plane.first().map_or(false, |count| *count > 0.0)
    }
}

impl MeshCorrections {
    /// Whether no frame has any correction
    pub fn is_empty(&self) -> bool {
        !self.frames.iter().any(|f| !f.is_empty())
    }
    pub fn clear(&mut self) {
        self.tables.clear();
        self.frames.clear();
    }
    /// The correction of a frame, `None` when it has none
    pub fn frame(&self, frame: usize) -> Option<&MeshFrame> {
        self.frames.get(frame).filter(|f| !f.is_empty())
    }
    pub fn has_mesh(&self, frame: usize) -> bool {
        self.frame(frame).map_or(false, |f| f.table.is_some())
    }
    pub fn has_focal_plane(&self, frame: usize) -> bool {
        self.frame(frame).map_or(false, |f| f.has_focal_plane())
    }
    fn table_of(&self, f: &MeshFrame) -> Option<&MeshTable> {
        self.tables.get(f.table? as usize)
    }
    /// Adds a table unless an equal one (by `key`) is already there, and returns its index. `cache` maps the keys of
    /// the tables added so far
    pub fn intern(
        &mut self,
        key: u32,
        cache: &mut BTreeMap<u32, u32>,
        build: impl FnOnce() -> MeshTable,
    ) -> u32 {
        *cache.entry(key).or_insert_with(|| {
            self.tables.push(build());
            (self.tables.len() - 1) as u32
        })
    }

    /// Appends a mesh block with the frame's geometry in its header, a bare header when the frame has no mesh
    fn push_block<T: Copy>(buf: &mut Vec<T>, block: &[T], f: &MeshFrame, conv: impl Fn(f64) -> T) {
        let start = buf.len();
        if block.len() >= MESH_HEADER {
            buf.extend_from_slice(block);
        } else {
            buf.push(conv(MESH_HEADER as f64));
            buf.extend(std::iter::repeat(conv(0.0)).take(MESH_HEADER - 1));
        }
        let geometry = [
            f.mesh_size.0,
            f.mesh_size.1,
            f.crop_origin.0,
            f.crop_origin.1,
            f.crop_size.0,
            f.crop_size.1,
        ];
        for (slot, v) in buf[start + 3..start + MESH_HEADER].iter_mut().zip(geometry) {
            *slot = conv(v);
        }
    }
    /// The focal plane table, or the 4-value header with a zero count the kernels probe when the frame has none
    fn push_focal_plane<T: Copy>(buf: &mut Vec<T>, f: &MeshFrame, conv: impl Fn(f64) -> T) {
        if f.focal_plane.len() >= 4 {
            buf.extend(f.focal_plane.iter().map(|v| conv(*v)));
        } else {
            buf.extend(std::iter::repeat(conv(0.0)).take(4));
        }
    }

    /// The kernels' buffer of a frame: `[inverse mesh][focal plane table][forward mesh]`, the forward mesh reduced to a
    /// single zero when the table doesn't need refining. The kernels probe that slot, and the GPU buffers keep stale
    /// data past what was uploaded, so it's always there. Empty when the frame has no correction
    pub fn kernel_buffer(&self, frame: usize) -> Vec<f32> {
        let Some(f) = self.frame(frame) else {
            return Vec::new();
        };
        let table = self.table_of(f);
        let inverse: &[f32] = table.map_or(&[], |t| t.inverse.as_slice());
        let refinement: &[f32] = table.map_or(&[], |t| t.refinement.as_slice());
        let mut buf = Vec::with_capacity(
            inverse.len().max(MESH_HEADER) + f.focal_plane.len().max(4) + refinement.len().max(1),
        );
        Self::push_block(&mut buf, inverse, f, |v| v as f32);
        Self::push_focal_plane(&mut buf, f, |v| v as f32);
        if refinement.is_empty() {
            buf.push(0.0);
        } else {
            Self::push_block(&mut buf, refinement, f, |v| v as f32);
        }
        buf
    }
    /// The camera's mesh of a frame with the frame's geometry in its header, followed by the focal plane table: what
    /// `undistort_points` applies. `None` when the frame has no correction
    pub fn forward_mesh(&self, frame: usize) -> Option<Vec<f64>> {
        let f = self.frame(frame)?;
        let forward: &[f64] = self.table_of(f).map_or(&[], |t| t.forward.as_slice());
        let mut buf =
            Vec::with_capacity(forward.len().max(MESH_HEADER) + f.focal_plane.len().max(4));
        Self::push_block(&mut buf, forward, f, |v| v);
        Self::push_focal_plane(&mut buf, f, |v| v);
        Some(buf)
    }

    /// Project files of older versions stored one buffer pair per frame: the camera's mesh as `[mesh block][focal plane
    /// table]` and the kernels' buffer as `[inverse block][focal plane table][forward block, in later versions]`, both
    /// with the frame's capture area in their headers. The frames whose mesh is the same share one table again. A pair
    /// that ends before the slots the kernels probe (the oldest files, a one-value placeholder of an intermediate build)
    /// is no correction, and a truncated focal plane table is dropped rather than read past
    pub fn from_legacy(frames: Vec<(Vec<f64>, Vec<f32>)>) -> Self {
        let mut out = Self::default();
        let mut cache = BTreeMap::new();
        for (forward, inverse) in frames {
            let frame =
                Self::legacy_frame(&forward, &inverse, &mut out, &mut cache).unwrap_or_default();
            out.frames.push(frame);
        }
        if out.is_empty() {
            out.clear();
        }
        out
    }
    fn legacy_frame(
        fwd: &[f64],
        inv: &[f32],
        out: &mut Self,
        cache: &mut BTreeMap<u32, u32>,
    ) -> Option<MeshFrame> {
        let block_len = |first: f64, len: usize| {
            let o = first as usize;
            (first >= MESH_HEADER as f64 && o <= len).then_some(o)
        };
        let of = block_len(*fwd.first()?, fwd.len())?;
        let oi = block_len(*inv.first()? as f64, inv.len())?;
        let focal_plane: Vec<f64> = match fwd.get(of) {
            Some(count) if *count > 0.0 => {
                let n = 4 + 2 * (*count as usize);
                if fwd.len() >= of + n {
                    fwd[of..of + n].to_vec()
                } else {
                    log::warn!("Truncated focal plane table in a project file, dropped");
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };
        let table = if of > MESH_HEADER && oi > MESH_HEADER {
            // The capture area is the frame's, not the table's
            let mut forward = fwd[..of].to_vec();
            for v in &mut forward[5..MESH_HEADER] {
                *v = 0.0;
            }
            let mut inverse = inv[..oi].to_vec();
            for v in &mut inverse[5..MESH_HEADER] {
                *v = 0.0;
            }
            // The forward block after the focal plane table, when the file has one
            let count = inv.get(oi).copied().unwrap_or(0.0).max(0.0) as usize;
            let at = oi + 4 + 2 * count;
            let refinement = match inv.get(at) {
                Some(len) if *len > MESH_HEADER as f32 && at + *len as usize <= inv.len() => {
                    let mut r = inv[at..at + *len as usize].to_vec();
                    for v in &mut r[5..MESH_HEADER] {
                        *v = 0.0;
                    }
                    r
                }
                _ => Vec::new(),
            };
            let mut hasher = crc32fast::Hasher::new();
            for v in &forward {
                hasher.update(&v.to_bits().to_le_bytes());
            }
            hasher.update(&[refinement.is_empty() as u8]);
            let key = hasher.finalize();
            Some(out.intern(key, cache, || MeshTable {
                forward,
                inverse,
                refinement,
            }))
        } else {
            None
        };
        Some(MeshFrame {
            table,
            mesh_size: (fwd[3], fwd[4]),
            crop_origin: (fwd[5], fwd[6]),
            crop_size: (fwd[7], fwd[8]),
            focal_plane,
        })
    }
}

// ------------- ReadOnlyFileMetadata -------------
// Make a thread-safe read-only wrapper for FileMetadata, because once it's read, it's never changed
#[derive(Clone)]
pub struct ReadOnlyFileMetadata(pub Arc<RwLock<FileMetadata>>);
impl Default for ReadOnlyFileMetadata {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(Default::default())))
    }
}
impl From<FileMetadata> for ReadOnlyFileMetadata {
    fn from(v: FileMetadata) -> Self {
        Self(Arc::new(RwLock::new(v)))
    }
}
impl ReadOnlyFileMetadata {
    pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, FileMetadata> {
        self.0.read()
    }
    pub fn set_raw_imu(&mut self, v: Vec<TimeIMU>) {
        self.0.write().raw_imu = v;
    }
    pub fn write(&self) -> parking_lot::RwLockWriteGuard<'_, FileMetadata> {
        self.0.write()
    }
}
impl serde::Serialize for ReadOnlyFileMetadata {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for ReadOnlyFileMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self(Arc::new(RwLock::new(FileMetadata::deserialize(
            deserializer,
        )?))))
    }
}
// ------------- ReadOnlyFileMetadata -------------

/// Decide whether a video's own built-in gyro is the trusted motion source and
/// must be kept instead of being overwritten by an external IMU. See
/// `FileMetadata::keep_video_gyro`. Pure predicate so it stays unit-testable
/// without a telemetry-parser `Input`.
///
/// - RED Komodo / Komodo-X: `is_komodo` is already true.
/// - Sony / Canon bodies that embedded gyro/quaternion samples: trusted when
///   motion is present (`has_gyro_samples`). Canon (R5 II / R6 II / R1 …) writes
///   a per-frame CNDM gyro burst frame-aligned to the video, like Sony, so it is
///   treated the same. A non-Komodo RED with samples is intentionally NOT trusted
///   here (its internal IMU is cleared separately).
/// - Blackmagic bodies recording `.braw`: trusted when motion is present. The IMU
///   samples are timestamped from the metadata track's own sample timestamps —
///   the same container timebase as the video track, minus half the frame readout
///   — so they are frame-aligned by construction and need no optical-flow sync.
///
/// The Blackmagic arm carries two extra conditions, neither of which is optional:
///
/// - **Brand is matched by prefix, not equality.** telemetry-parser's
///   `camera_type()` returns `"Blackmagic RAW"` when the model could not be
///   identified, which `== "Blackmagic"` would miss. The same comparison is what
///   excludes a Video Assist recording, whose `camera_type()` returns the *source*
///   camera's manufacturer (e.g. `"Panasonic"`) instead.
/// - **The container must be `.braw`.** A Video Assist recording is also a `.braw`
///   container, so the container alone cannot separate the two; and the brand
///   alone would pull in Blackmagic ProRes `.mov` and CinemaDNG, which are
///   deliberately out of scope (they keep the external-IMU override path).
///
/// No body is special-cased, including `Micro Studio Camera 4K G2` — the one
/// Blackmagic model telemetry-parser flags as `has_accurate_timestamps == false`.
/// Once this predicate is true the sync policy short-circuits before that flag is
/// ever read, so it survives only for display and project export.
pub(crate) fn compute_keep_video_gyro(
    is_komodo: bool,
    camera_type: &str,
    has_gyro_samples: bool,
    is_braw_container: bool,
) -> bool {
    is_komodo
        || ((camera_type == "Sony" || camera_type == "Canon") && has_gyro_samples)
        || (camera_type.starts_with("Blackmagic") && has_gyro_samples && is_braw_container)
}

/// Whether in-camera stabilization prevents Gyroflow from producing a correct
/// result for a clip. See `classify_in_camera_stabilization`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StabilizationVerdict {
    /// The stabilizer flag is absent or reports off — nothing to arbitrate.
    NotStabilized,
    /// Stabilizer on, and the clip carries compensation data Gyroflow subtracts
    /// before applying its own correction.
    CompensationAvailable,
    /// The camera reports stabilization, but the signal is known to produce
    /// false positives and is retained for diagnostics only.
    IgnoredUntrustedSignal,
    /// Stabilizer on, but the mounted lens reports no OSS metadata, so the
    /// optical part of the correction can never be subtracted.
    UnsupportedLens,
    /// Stabilizer on with no compensation data at all.
    NoCompensation,
}

impl StabilizationVerdict {
    /// True when the clip cannot be stabilized correctly and must be skipped.
    pub fn blocks_processing(self) -> bool {
        matches!(self, Self::UnsupportedLens | Self::NoCompensation)
    }

    /// Stable identifier used for the queue skip reason and user-facing copy.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStabilized => "not_stabilized",
            Self::CompensationAvailable => "compensation_available",
            Self::IgnoredUntrustedSignal => "ignored_untrusted_signal",
            Self::UnsupportedLens => "unsupported_lens",
            Self::NoCompensation => "no_compensation",
        }
    }
}

/// Decide whether in-camera stabilization (body IBIS, lens OSS, electronic)
/// blocks processing for a clip. Pure predicate so it stays unit-testable
/// without a telemetry-parser `Input`.
///
/// `stabilizer_on` comes from `TagId::ImageStabilizer`, whose decoders read
/// `raw == 0` as "on". `ibis_points` / `ois_points` are the sample counts
/// collected by `sony::stab_collect`, and `ois_sentinel` is
/// `sony::is_unsupported_lens_sentinel` over the raw OSS stream.
///
/// Four constraints shape the order of the checks:
///
/// - The stabilizer flag is consulted first and short-circuits. The
///   overwhelming majority of clips report off, and they must reach the same
///   behaviour as before this gate existed without any compensation lookup.
/// - Canon's CNDM flag is not a trustworthy indication that image stabilization
///   affected the recorded frames. It is retained in `additional_data` for
///   diagnostics but never blocks processing, including for first-party RF/EF
///   lenses.
/// - Only Sony records compensation streams. Other supported brands expose just
///   the on/off flag, so "on" is equivalent to "cannot process" — those callers
///   pass zeroed counts and never read `camera_stab_data`.
/// - The OSS sentinel is tested *before* the point counts. `stab_collect`
///   pushes the sentinel's `-1` into `ISTemp::ois_x`, so a sentinel-only clip
///   has `ois_points == 1` and would otherwise read as real compensation. A
///   sentinel blocks regardless of IBIS: the lens is optically moving the image
///   and that part is unrecoverable even when body IBIS is fully described.
pub(crate) fn classify_in_camera_stabilization(
    stabilizer_on: Option<bool>,
    camera_type: &str,
    ibis_points: usize,
    ois_points: usize,
    ois_sentinel: bool,
) -> StabilizationVerdict {
    if stabilizer_on != Some(true) {
        return StabilizationVerdict::NotStabilized;
    }
    if camera_type == "Canon" {
        return StabilizationVerdict::IgnoredUntrustedSignal;
    }
    if camera_type != "Sony" {
        return StabilizationVerdict::NoCompensation;
    }
    if ois_sentinel {
        return StabilizationVerdict::UnsupportedLens;
    }
    if ibis_points > 0 || ois_points > 0 {
        return StabilizationVerdict::CompensationAvailable;
    }
    StabilizationVerdict::NoCompensation
}

#[cfg(test)]
mod tests {
    use super::super::Quat64;
    use super::*;

    #[test]
    fn stabilization_off_short_circuits_before_any_compensation_lookup() {
        // The overwhelming majority of clips land here. Non-zero compensation
        // counts are passed deliberately: reaching NotStabilized proves the
        // flag is consulted first and nothing downstream is read.
        assert_eq!(
            classify_in_camera_stabilization(Some(false), "Sony", 720, 720, true),
            StabilizationVerdict::NotStabilized
        );
        assert!(!StabilizationVerdict::NotStabilized.blocks_processing());
    }

    #[test]
    fn stabilization_flag_absent_is_not_stabilized() {
        // GoPro / Blackmagic never emit TagId::ImageStabilizer.
        assert_eq!(
            classify_in_camera_stabilization(None, "GoPro", 0, 0, false),
            StabilizationVerdict::NotStabilized
        );
    }

    #[test]
    fn non_sony_non_canon_with_stabilizer_on_always_blocks() {
        // Nikon / Fujifilm / Panasonic expose only the on/off flag — there is
        // no compensation stream to subtract, so "on" means "cannot process".
        // Compensation counts are non-zero to prove the Sony gate rejects them
        // before the counts are ever consulted.
        let verdict = classify_in_camera_stabilization(Some(true), "Nikon", 720, 720, false);
        assert_eq!(verdict, StabilizationVerdict::NoCompensation);
        assert!(verdict.blocks_processing());
    }

    #[test]
    fn canon_stabilizer_signal_is_untrusted_and_never_blocks() {
        // Canon's CNDM flag produces false positives even with first-party RF/EF
        // lenses. Keep the raw flag for diagnostics, but never let it skip a job.
        let verdict = classify_in_camera_stabilization(Some(true), "Canon", 0, 0, false);
        assert_eq!(verdict.as_str(), "ignored_untrusted_signal");
        assert!(!verdict.blocks_processing());
    }

    #[test]
    fn sony_with_ibis_compensation_is_allowed() {
        // Tier 2: body IBIS described, lens OSS absent (A7S3 baseline).
        let verdict = classify_in_camera_stabilization(Some(true), "Sony", 720, 0, false);
        assert_eq!(verdict, StabilizationVerdict::CompensationAvailable);
        assert!(!verdict.blocks_processing());
    }

    #[test]
    fn sony_with_both_compensation_streams_is_allowed() {
        // Tier 3: both streams described.
        assert_eq!(
            classify_in_camera_stabilization(Some(true), "Sony", 720, 720, false),
            StabilizationVerdict::CompensationAvailable
        );
    }

    #[test]
    fn sony_without_any_compensation_blocks() {
        // Tier 1: stabilizer engaged but nothing recorded (A6400 baseline).
        let verdict = classify_in_camera_stabilization(Some(true), "Sony", 0, 0, false);
        assert_eq!(verdict, StabilizationVerdict::NoCompensation);
        assert!(verdict.blocks_processing());
    }

    #[test]
    fn oss_sentinel_blocks_even_though_it_inflates_the_ois_count() {
        // stab_collect pushes the sentinel's -1 into ois_x, so a sentinel-only
        // clip arrives here with ois_points == 1. Testing the emptiness of the
        // OSS stream instead of the sentinel would let exactly the clip that
        // most needs blocking through.
        let verdict = classify_in_camera_stabilization(Some(true), "Sony", 0, 1, true);
        assert_eq!(verdict, StabilizationVerdict::UnsupportedLens);
        assert!(verdict.blocks_processing());
    }

    #[test]
    fn oss_sentinel_blocks_regardless_of_ibis_data() {
        // The lens is optically moving the image and never reports by how much.
        // A fully described body IBIS stream does not make that recoverable.
        assert_eq!(
            classify_in_camera_stabilization(Some(true), "Sony", 720, 1, true),
            StabilizationVerdict::UnsupportedLens
        );
    }

    #[test]
    fn keep_video_gyro_sony_with_samples_is_true() {
        assert!(compute_keep_video_gyro(false, "Sony", true, false));
    }

    #[test]
    fn keep_video_gyro_sony_without_samples_is_false() {
        assert!(!compute_keep_video_gyro(false, "Sony", false, false));
    }

    #[test]
    fn keep_video_gyro_komodo_is_true() {
        // RED Komodo is trusted regardless of the Sony clause / sample presence.
        assert!(compute_keep_video_gyro(true, "RED", false, false));
    }

    #[test]
    fn keep_video_gyro_canon_with_samples_is_true() {
        // Canon bodies with embedded per-frame gyro (R5 II / R6 II / R1 …) are
        // trusted, same as Sony.
        assert!(compute_keep_video_gyro(false, "Canon", true, false));
    }

    #[test]
    fn keep_video_gyro_canon_without_samples_is_false() {
        // A Canon clip with no embedded motion keeps the external-IMU override path.
        assert!(!compute_keep_video_gyro(false, "Canon", false, false));
    }

    #[test]
    fn keep_video_gyro_container_flag_does_not_affect_other_arms() {
        // The container argument exists only for the Blackmagic arm. Komodo / Sony /
        // Canon must reach the same verdict either way, so a future caller that gets
        // the container detection wrong cannot silently flip them.
        for is_braw in [false, true] {
            assert!(compute_keep_video_gyro(true, "RED", false, is_braw));
            assert!(compute_keep_video_gyro(false, "Sony", true, is_braw));
            assert!(compute_keep_video_gyro(false, "Canon", true, is_braw));
            assert!(!compute_keep_video_gyro(false, "Sony", false, is_braw));
            assert!(!compute_keep_video_gyro(false, "Canon", false, is_braw));
        }
    }

    #[test]
    fn keep_video_gyro_blackmagic_braw_with_samples_is_true() {
        // A Blackmagic body recording .braw (BMCC 6K, BMPCC, URSA, Pyxis …) writes
        // its IMU into the metadata track on the container timebase, so it is
        // frame-aligned by construction.
        assert!(compute_keep_video_gyro(false, "Blackmagic", true, true));
    }

    #[test]
    fn keep_video_gyro_blackmagic_raw_prefix_is_true() {
        // telemetry-parser returns "Blackmagic RAW" when the model could not be
        // identified. The brand test must be a prefix match, not equality, or these
        // clips silently fall back to the external-IMU path.
        assert!(compute_keep_video_gyro(false, "Blackmagic RAW", true, true));
    }

    #[test]
    fn keep_video_gyro_blackmagic_without_samples_is_false() {
        // Older Blackmagic bodies have no IMU at all; they keep the external-IMU
        // override path.
        assert!(!compute_keep_video_gyro(false, "Blackmagic", false, true));
    }

    #[test]
    fn keep_video_gyro_blackmagic_non_braw_container_is_false() {
        // Scope boundary: Blackmagic ProRes .mov and CinemaDNG carry the same
        // mogy/moac samples but are deliberately left on the external-IMU path.
        assert!(!compute_keep_video_gyro(false, "Blackmagic", true, false));
        assert!(!compute_keep_video_gyro(false, "Blackmagic RAW", true, false));
    }

    #[test]
    fn keep_video_gyro_video_assist_source_brand_is_false() {
        // A Video Assist recording is also a .braw container, but camera_type() is
        // the *source* camera's manufacturer. Only the brand test separates it, so
        // this locks in why the container flag alone is not sufficient.
        assert!(!compute_keep_video_gyro(false, "Panasonic", true, true));
        assert!(!compute_keep_video_gyro(false, "Fujifilm", true, true));
    }

    #[test]
    fn keep_video_gyro_other_cameras_are_false() {
        // Non-Komodo RED (even with samples) and unrelated bodies are not trusted
        // by the generic clause (RED's own IMU is cleared separately). The container
        // flag must not let any of them through either.
        assert!(!compute_keep_video_gyro(false, "RED", true, false));
        assert!(!compute_keep_video_gyro(false, "RED", true, true));
        assert!(!compute_keep_video_gyro(false, "Nikon", true, false));
        assert!(!compute_keep_video_gyro(false, "Nikon", true, true));
    }

    #[test]
    fn thin_preserves_keep_video_gyro_after_stripping_motion() {
        let mut md = FileMetadata {
            keep_video_gyro: true,
            ..Default::default()
        };
        md.quaternions.insert(0, Quat64::identity());
        assert!(!md.quaternions.is_empty());

        // thin() strips raw_imu/quaternions but must carry the trusted-gyro flag,
        // which is why the flag is stored rather than derived via has_motion().
        let thin = md.thin();
        assert!(thin.keep_video_gyro, "thin() must preserve keep_video_gyro");
        assert!(thin.quaternions.is_empty(), "thin() must strip quaternions");
        assert!(thin.raw_imu.is_empty(), "thin() must strip raw_imu");
    }

    #[test]
    fn thin_preserves_canon_auto_lens_profile() {
        // The deferred Canon opencv_standard lens must survive thin() (used when
        // caching/cloning keep_video_gyro metadata for batch jobs), otherwise the
        // batch apply could not activate it.
        let mut md = FileMetadata::default();
        md.canon_auto_lens_profile =
            Some(serde_json::json!({ "distortion_model": "opencv_standard" }));
        let thin = md.thin();
        assert!(
            thin.canon_auto_lens_profile.is_some(),
            "thin() must preserve canon_auto_lens_profile"
        );
    }
}
