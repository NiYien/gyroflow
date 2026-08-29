// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::{TimeIMU, TimeQuat, TimeVec, splines};
use crate::camera_identifier::CameraIdentifier;
use crate::stabilization_params::ReadoutDirection;

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LensParams {
    pub focal_length: Option<f32>,               // millimeters
    pub pixel_pitch: Option<(u32, u32)>,         // nanometers
    pub sensor_size_px: Option<(u32, u32)>,      // pixels
    pub capture_area_origin: Option<(f32, f32)>, // pixels
    pub capture_area_size: Option<(f32, f32)>,   // pixels
    pub pixel_focal_length: Option<f32>,         // pixels
    pub distortion_coefficients: Vec<f64>,
    pub focus_distance: Option<f32>,
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CameraStabData {
    pub offset: f64,
    pub sensor_size: (u32, u32),
    pub crop_area: (f32, f32, f32, f32),
    pub pixel_pitch: (u32, u32),
    pub ibis_spline: splines::CatmullRom<nalgebra::Vector3<f64>>,
    pub ois_spline: splines::CatmullRom<nalgebra::Vector3<f64>>,
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
    pub mesh_correction: Vec<(Vec<f64>, Vec<f32>)>,
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
        }
    }
    pub fn has_motion(&self) -> bool {
        !self.raw_imu.is_empty() || !self.quaternions.is_empty()
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
    use super::*;
    use super::super::Quat64;

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
