// SPDX-License-Identifier: GPL-3.0-or-later

//! Optical analysis for render queue jobs: the experimental panel's settings as the queue keeps them, and the step the
//! render worker runs between the sync and the project write / encode (see `RenderQueue::render_job`).

use gyroflow_core::gyro_source::GyroSource;
use gyroflow_core::synchronization::optical_analysis::{context_checksum, measurement_params};
use gyroflow_core::StabilizationManager;

pub const CORRECTION: &str = "correction";
pub const TRANSLATION: &str = "translation";
pub const RECONSTRUCTION: &str = "reconstruction";

/// Why a translation or a reconstruction did not even get analyzed. The panel shows the same text in this situation
pub const NEEDS_MOTION: &str = "Needs motion data from the file";
/// An analysis that finished without error but left a ticked item without a result that applies
pub const NO_RESULT: &str = "The analysis gave no usable result";

/// The experimental panel's settings, pushed to the queue on every user edit. The values are what the core setters
/// take. Missing fields take the core's defaults, so a hand-written `{"translation":true}` works too
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct QueueOpticalSettings {
    pub correction: bool,
    pub strength: f64,
    pub ignore_file_motion: bool,
    pub translation: bool,
    pub translation_reference: f64,
    /// Seconds
    pub translation_smoothness: f64,
    pub translation_along_axis: bool,
    pub reconstruction: bool,
}

impl Default for QueueOpticalSettings {
    fn default() -> Self {
        Self {
            correction: false,
            strength: 0.5,
            ignore_file_motion: false,
            translation: false,
            translation_reference: 1.0,
            translation_smoothness: 1.0,
            translation_along_axis: true,
            reconstruction: false,
        }
    }
}

impl QueueOpticalSettings {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Whether any of the three items is ticked
    pub fn any_ticked(&self) -> bool {
        self.correction || self.translation || self.reconstruction
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueueOpticalOutcome {
    /// Every ticked item applies. `analyzed`: this step ran an analysis for them
    Applied { analyzed: bool },
    /// These ticked items did not apply and were turned off: the job is processed without them
    Fallback { analyzed: bool, items: Vec<&'static str>, reason: String },
    /// The clip needs its in-camera stabilization compensated and the reconstruction does not apply: it must not be
    /// processed at all, the basic stabilization would compensate it twice
    Skip { reason: String },
    /// The queue stopped or paused the analysis
    Cancelled,
}

impl QueueOpticalOutcome {
    /// What the queue row shows (`optical_notice`, read by `App.qml::getReadableError`). Empty: nothing to tell
    pub fn notice(&self) -> String {
        match self {
            Self::Fallback { items, reason, .. } => format!("optical_fallback:{}:{}", items.join(","), reason),
            Self::Skip { reason } => format!("optical_skipped:{RECONSTRUCTION}:{reason}"),
            Self::Applied { .. } | Self::Cancelled => String::new(),
        }
    }
}

/// The job's additional data with the notice added as `optical_notice`, so that the project written next records why an
/// item is off. Unchanged when it isn't a JSON object
pub fn with_notice(additional_data: &str, notice: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(additional_data) {
        Ok(serde_json::Value::Object(mut obj)) => {
            obj.insert("optical_notice".into(), notice.into());
            serde_json::Value::Object(obj).to_string()
        }
        _ => additional_data.to_owned(),
    }
}

/// The safety net's reason: a clip that needs the reconstruction reached the analysis step with it unticked
pub const RECONSTRUCTION_OFF: &str = "Reconstruct in-camera stabilization is not ticked";

/// Applies `settings` to the job's manager, analyzes what they ask for and doesn't apply yet, and turns off what still
/// doesn't apply after that. `requires_compensation`: the clip was recorded with in-camera stabilization on and without
/// its compensation data, see `RenderQueue::job_original_stabilization_requirement`. `analyze` is
/// `rendering::analyze_optically` in production
pub fn run_queue_optical(
    job_id: u32,
    stab: &StabilizationManager,
    settings: &QueueOpticalSettings,
    requires_compensation: bool,
    analyze: impl FnOnce(&StabilizationManager) -> Result<(), String>,
) -> QueueOpticalOutcome {
    let changed = apply_settings(job_id, stab, settings);
    stab.refresh_optical_correction();
    let ui = *stab.optical_ui.read();
    let file_motion = {
        let gyro = stab.gyro.read();
        gyro.has_motion() && !gyro.ignores_file_motion()
    };
    let requested: Vec<&'static str> = [(ui.correction_enabled, CORRECTION), (ui.translation_enabled, TRANSLATION), (ui.stab_enabled, RECONSTRUCTION)]
        .into_iter()
        .filter_map(|(ticked, item)| ticked.then_some(item))
        .collect();
    // A translation and a reconstruction are measured against the file's motion data: without it there's nothing to analyze
    let (unmeasurable, measurable): (Vec<&'static str>, Vec<&'static str>) =
        requested.iter().copied().partition(|item| *item != CORRECTION && !file_motion);
    let pending: Vec<&'static str> = measurable.into_iter().filter(|item| !applies(stab, item)).collect();
    let mut failed: Vec<(&'static str, String)> = unmeasurable.into_iter().map(|item| (item, NEEDS_MOTION.to_string())).collect();

    let analyzed = !pending.is_empty();
    if analyzed {
        ::log::info!(target: "queue.optical", "[queue-optical] job={job_id} analyzing {}", pending.join(","));
        let started = std::time::Instant::now();
        match analyze(stab) {
            Err(e) if e == "Cancelled" => {
                ::log::info!(target: "queue.optical", "[queue-optical] job={job_id} analysis cancelled after {} ms", started.elapsed().as_millis());
                return QueueOpticalOutcome::Cancelled;
            }
            Err(e) => {
                ::log::warn!(target: "queue.optical", "[queue-optical] job={job_id} analysis failed after {} ms: {e}", started.elapsed().as_millis());
                failed.extend(pending.iter().map(|item| (*item, e.clone())));
            }
            Ok(()) => {
                stab.recompute_blocking();
                ::log::info!(target: "queue.optical", "[queue-optical] job={job_id} analysis done in {} ms", started.elapsed().as_millis());
                failed.extend(pending.iter().filter(|item| !applies(stab, item)).map(|item| (*item, NO_RESULT.to_string())));
            }
        }
    }
    let order = |item: &&str| [CORRECTION, TRANSLATION, RECONSTRUCTION].iter().position(|x| x == item);
    failed.sort_by_key(|(item, _)| order(item));

    // Basic stabilization would compensate the in-camera stabilization a second time
    if requires_compensation && !applies(stab, RECONSTRUCTION) {
        let reason = failed.iter().find(|(item, _)| *item == RECONSTRUCTION).map(|(_, reason)| reason.clone())
            .unwrap_or_else(|| RECONSTRUCTION_OFF.to_string());
        ::log::warn!(target: "queue.optical", "[queue-optical] job={job_id} skipped: the clip needs its in-camera stabilization reconstructed ({reason})");
        return QueueOpticalOutcome::Skip { reason };
    }
    if failed.is_empty() {
        if changed && !analyzed {
            stab.recompute_blocking();
        }
        ::log::info!(target: "queue.optical", "[queue-optical] job={job_id} applies: [{}] analyzed={analyzed} settings_changed={changed}", requested.join(","));
        return QueueOpticalOutcome::Applied { analyzed };
    }

    for (item, _) in &failed {
        match *item {
            CORRECTION => {
                stab.set_optical_correction_enabled(false);
                stab.set_ignore_file_motion(false);
            }
            TRANSLATION => stab.set_translation_stabilization_enabled(false),
            _ => stab.set_stab_reconstruction_enabled(false),
        }
    }
    stab.recompute_blocking();
    let items: Vec<&'static str> = failed.iter().map(|(item, _)| *item).collect();
    let mut reasons: Vec<String> = Vec::new();
    for (_, reason) in failed {
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    let reason = reasons.join("; ");
    ::log::warn!(target: "queue.optical", "[queue-optical] job={job_id} processed without {}: {reason}", items.join(","));
    QueueOpticalOutcome::Fallback { analyzed, items, reason }
}

/// The order of `MotionData.qml::analyzeOpticalModes`: the core setters keep the modes exclusive, the last tick wins as on
/// the panel. Only what differs is set, so that an unchanged job is not invalidated. True when anything changed
fn apply_settings(job_id: u32, stab: &StabilizationManager, s: &QueueOpticalSettings) -> bool {
    let state = |stab: &StabilizationManager| {
        let (ui, strength) = (*stab.optical_ui.read(), stab.optical_settings.read().strength);
        (ui, strength, stab.gyro.read().ignores_file_motion())
    };
    let before = state(stab);
    let (ui, strength, ignoring) = before;
    if ui.correction_enabled != s.correction {
        stab.set_optical_correction_enabled(s.correction);
    }
    let ignore = resolve(s).ignore_file_motion;
    if ignoring != ignore {
        stab.set_ignore_file_motion(ignore);
    }
    if strength != s.strength.clamp(0.0, 1.0) && stab.set_optical_correction_strength(s.strength) {
        // Kept measurements refit in a fraction of a second; without them only an analysis applies the new strength
        if let Err(e) = stab.refit_optical_correction() {
            ::log::warn!(target: "queue.optical", "[queue-optical] job={job_id} refit with strength {} failed: {e}", s.strength);
        }
    }
    let t = ui.translation_settings;
    if t.reference != s.translation_reference.clamp(0.0, 2.0) {
        stab.set_translation_reference(s.translation_reference);
    }
    if t.smoothness_s != s.translation_smoothness.clamp(0.1, 10.0) {
        stab.set_translation_smoothness(s.translation_smoothness);
    }
    if t.along_axis != s.translation_along_axis {
        stab.set_translation_along_axis(s.translation_along_axis);
    }
    if stab.optical_ui.read().translation_enabled != s.translation {
        stab.set_translation_stabilization_enabled(s.translation);
    }
    if stab.optical_ui.read().stab_enabled != s.reconstruction {
        stab.set_stab_reconstruction_enabled(s.reconstruction);
    }
    before != state(stab)
}

/// What `apply_settings` leaves switched on: the core setters keep the modes exclusive and a reconstruction turns the
/// other two off; ignoring the file's motion is part of the correction, and a reconstruction needs the motion
struct Resolved {
    correction: bool,
    translation: bool,
    reconstruction: bool,
    ignore_file_motion: bool,
}

fn resolve(s: &QueueOpticalSettings) -> Resolved {
    Resolved {
        correction: s.correction && !s.reconstruction,
        translation: s.translation && !s.reconstruction,
        reconstruction: s.reconstruction,
        ignore_file_motion: s.correction && s.ignore_file_motion && !s.reconstruction,
    }
}

impl Resolved {
    fn items(&self) -> Vec<&'static str> {
        [(self.correction, CORRECTION), (self.translation, TRANSLATION), (self.reconstruction, RECONSTRUCTION)]
            .into_iter()
            .filter_map(|(on, item)| on.then_some(item))
            .collect()
    }
}

/// stabilize-flow-optical-analysis: whether a ticked item of `settings` lacks a result that applies in the job's
/// current context, so that `run_queue_optical` would analyze it (or fall back for want of motion data). Leaves the
/// manager as it is
pub fn needs_analysis(stab: &StabilizationManager, settings: &QueueOpticalSettings) -> bool {
    let resolved = resolve(settings);
    let items = resolved.items();
    if items.is_empty() {
        return false;
    }
    // Setting the file's motion aside or bringing it back reintegrates: only the analysis step can tell what applies then
    if stab.gyro.read().ignores_file_motion() != resolved.ignore_file_motion {
        return true;
    }
    // Fresh: the cached context lags behind sync points set without a recompute. Takes the gyro lock itself
    let context = context_checksum(&measurement_params(stab));
    let strength = settings.strength.clamp(0.0, 1.0);
    let gyro = stab.gyro.read();
    items.iter().any(|item| !result_applies(&gyro, context, item, strength))
}

/// stabilize-flow-optical-analysis: whether `run_queue_optical` would change the manager's settings
/// (`settings_changed`), compared as the setters clamp them. Leaves the manager as it is
pub fn settings_differ(stab: &StabilizationManager, s: &QueueOpticalSettings) -> bool {
    let r = resolve(s);
    let (ui, strength) = (*stab.optical_ui.read(), stab.optical_settings.read().strength);
    let ignoring = stab.gyro.read().ignores_file_motion();
    let t = ui.translation_settings;
    (ui.correction_enabled, ui.translation_enabled, ui.stab_enabled) != (r.correction, r.translation, r.reconstruction)
        || ignoring != r.ignore_file_motion
        || strength != s.strength.clamp(0.0, 1.0)
        || t.reference != s.translation_reference.clamp(0.0, 2.0)
        || t.smoothness_s != s.translation_smoothness.clamp(0.1, 10.0)
        || t.along_axis != s.translation_along_axis
}

/// stabilize-flow-optical-analysis: whether a job whose batch sync is final needs an analysis pass for the project it
/// hands to the plugins: something is ticked, and an item lacks its result or the manager carries other settings
pub fn stabilize_pass_needed(stab: &StabilizationManager, settings: &QueueOpticalSettings) -> bool {
    settings.any_ticked() && (needs_analysis(stab, settings) || settings_differ(stab, settings))
}

/// Whether the result for `item` is there and measured on what the job has now, switched on or not: the uncorrected
/// motion and `context` (sync, lens, frame timing), for a translation or a reconstruction the file's motion data. A
/// correction also has to be fitted with `strength`. Shared by `applies` and `needs_analysis`
fn result_applies(gyro: &GyroSource, context: u64, item: &str, strength: f64) -> bool {
    let file_motion = gyro.has_motion() && !gyro.ignores_file_motion();
    let uncorrected = gyro.optical_uncorrected_checksum;
    match item {
        CORRECTION => gyro.optical_correction.as_ref()
            .is_some_and(|c| c.measured_on(uncorrected, context) && c.settings.strength == strength),
        TRANSLATION => file_motion && gyro.optical_translation.as_ref().is_some_and(|t| {
            t.has_valid_geometry() && !t.samples.is_empty() && t.quats_checksum == uncorrected && t.context_checksum == context
        }),
        _ => file_motion && gyro.optical_stab.as_ref().is_some_and(|s| s.measured_on(uncorrected, context) && s.has_table()),
    }
}

/// Whether the result for a ticked item is there and applies, as the render would use it
fn applies(stab: &StabilizationManager, item: &str) -> bool {
    stab.refresh_optical_correction();
    let strength = stab.optical_settings.read().strength;
    let gyro = stab.gyro.read();
    let enabled = match item {
        CORRECTION => gyro.optical_correction.as_ref().is_some_and(|c| c.enabled),
        TRANSLATION => gyro.optical_translation.as_ref().is_some_and(|t| t.enabled),
        _ => gyro.optical_stab.as_ref().is_some_and(|s| s.enabled),
    };
    // After the refresh the cached context is the current one
    enabled && result_applies(&gyro, gyro.optical_context, item, strength)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gyroflow_core::gyro_source::{
        optical_correction::checksum, OpticalCorrection, OpticalStabReconstruction, OpticalTranslation, Quat64,
        TimeQuat, TranslationSample,
    };
    use gyroflow_core::synchronization::optical_analysis::{context_checksum, measurement_params};
    use std::cell::Cell;

    /// A 10 s clip with motion data (still), optionally recorded with in-camera stabilization on and no compensation
    fn manager(requires_compensation: bool) -> StabilizationManager {
        let stab = StabilizationManager::default();
        {
            let mut params = stab.params.write();
            params.frame_count = 300;
            params.duration_ms = 10_000.0;
            params.fps = 30.0;
        }
        stab.set_size(1920, 1080);
        stab.set_output_size(1920, 1080);
        stab.input_file.write().url = "file:///queue-optical.mp4".to_owned();
        {
            let mut gyro = stab.gyro.write();
            gyro.duration_ms = 10_000.0;
            let quats: TimeQuat = [0, 10_000_000].into_iter().map(|t| (t, Quat64::identity())).collect();
            gyro.file_metadata.write().quaternions = quats;
            if requires_compensation {
                gyro.file_metadata.write().additional_data = serde_json::json!({ "stabilization_blocks_processing": true });
            }
            gyro.integrate();
        }
        stab
    }

    /// The motion data the results were measured on: without any correction
    fn uncorrected_checksum(stab: &StabilizationManager) -> u64 {
        let mut gyro = stab.gyro.read().clone();
        if gyro.optical_correction.take().is_some() { gyro.integrate(); }
        checksum(&gyro.quaternions)
    }

    fn install_correction(stab: &StabilizationManager) {
        let correction = OpticalCorrection {
            enabled: stab.optical_ui.read().correction_enabled,
            settings: *stab.optical_settings.read(),
            start_us: 0.0,
            spacing_us: 100_000.0,
            coeffs: vec![[0.0005, 0.0, 0.0]; 103],
            quats_checksum: uncorrected_checksum(stab),
            context_checksum: context_checksum(&measurement_params(stab)),
            frames: 300,
            measured_frames: 299,
            ..Default::default()
        };
        stab.set_optical_correction(Some(correction));
    }

    fn install_translation(stab: &StabilizationManager) {
        let samples = (0..300).map(|i| TranslationSample {
            timestamp_us: (i as f64 * 1_000_000.0 / 30.0).round() as i64,
            position: [0.01 * (i as f32 * 0.6).sin(), 0.0, 0.0],
            ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 2.0, segment: 0,
            camera_to_world: [1.0, 0.0, 0.0, 0.0], focal_length_over_short_side: 1.0,
            layer_motion: [0.001, 0.0], far_beta: 1.0, weight: 1.0, layer_scale_rate: 0.0,
        }).collect();
        let ui = *stab.optical_ui.read();
        let mut translation = OpticalTranslation::new(samples, ui.translation_settings);
        translation.enabled = ui.translation_enabled;
        translation.quats_checksum = uncorrected_checksum(stab);
        translation.context_checksum = context_checksum(&measurement_params(stab));
        translation.frames = 300;
        translation.measured_frames = 300;
        stab.gyro.write().optical_translation = Some(translation);
        stab.recompute_gyro();
    }

    fn install_reconstruction(stab: &StabilizationManager) {
        let mut s = OpticalStabReconstruction::default();
        s.enabled = stab.optical_ui.read().stab_enabled;
        s.start_us = -100_000.0;
        s.spacing_us = 100_000.0;
        s.coeffs = vec![[0.001, -0.0003, 0.0001]; 104];
        s.cutoff_hz = 0.3;
        s.quats_checksum = uncorrected_checksum(stab);
        s.context_checksum = context_checksum(&measurement_params(stab));
        stab.gyro.write().optical_stab = Some(s);
        stab.recompute_gyro();
    }

    fn translation_active(stab: &StabilizationManager) -> bool {
        stab.refresh_optical_correction();
        stab.gyro.read().optical_translation.as_ref().is_some_and(|t| t.is_active())
    }
    fn reconstruction_active(stab: &StabilizationManager) -> bool {
        stab.refresh_optical_correction();
        stab.gyro.read().optical_stab.as_ref().is_some_and(|s| s.is_active())
    }
    fn correction_active(stab: &StabilizationManager) -> bool {
        stab.refresh_optical_correction();
        let gyro = stab.gyro.read();
        gyro.optical_correction_applied && gyro.optical_correction.as_ref().is_some_and(|c| c.enabled)
    }

    fn settings(json: serde_json::Value) -> QueueOpticalSettings {
        QueueOpticalSettings::from_json(&json.to_string()).unwrap()
    }

    #[test]
    fn missing_fields_take_the_core_defaults() {
        let s = settings(serde_json::json!({ "translation": true }));
        assert_eq!(s, QueueOpticalSettings { translation: true, ..Default::default() });
        assert_eq!(s.strength, gyroflow_core::gyro_source::OpticalCorrectionSettings::default().strength);
        let ui = gyroflow_core::gyro_source::OpticalTranslationSettings::default();
        assert_eq!((s.translation_reference, s.translation_smoothness, s.translation_along_axis), (ui.reference, ui.smoothness_s, ui.along_axis));
        assert!(QueueOpticalSettings::from_json("{not json").is_err());
        assert!(QueueOpticalSettings::from_json("[1]").is_err());
    }

    #[test]
    fn notices_encode_items_and_reason() {
        let fallback = QueueOpticalOutcome::Fallback { analyzed: true, items: vec![CORRECTION, TRANSLATION], reason: "a: b".into() };
        assert_eq!(fallback.notice(), "optical_fallback:correction,translation:a: b");
        assert_eq!(QueueOpticalOutcome::Skip { reason: "x".into() }.notice(), "optical_skipped:reconstruction:x");
        assert_eq!(QueueOpticalOutcome::Applied { analyzed: true }.notice(), "");
        assert_eq!(QueueOpticalOutcome::Cancelled.notice(), "");
    }

    #[test]
    fn the_written_project_records_the_notice() {
        let data = with_notice(r#"{"output":{"codec":"x"}}"#, "optical_fallback:translation:a: b");
        let data: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(data["optical_notice"], "optical_fallback:translation:a: b");
        assert_eq!(data["output"]["codec"], "x");
        assert_eq!(with_notice("", "n"), "");
        assert_eq!(with_notice("[1]", "n"), "[1]");
    }

    #[test]
    fn settings_reach_the_manager_like_the_panel_submits_them() {
        let stab = manager(false);
        let s = settings(serde_json::json!({
            "translation": true, "translation_reference": 0.6, "translation_smoothness": 2.5, "translation_along_axis": false,
            "strength": 0.3,
        }));
        let calls = Cell::new(0);
        run_queue_optical(1, &stab, &s, false, |_| { calls.set(calls.get() + 1); Err("tracking failed".into()) });
        assert_eq!(calls.get(), 1);
        let ui = *stab.optical_ui.read();
        assert!(!ui.correction_enabled, "the core default (true) must not survive an unticked correction");
        assert!(!ui.stab_enabled);
        assert_eq!((ui.translation_settings.reference, ui.translation_settings.smoothness_s, ui.translation_settings.along_axis), (0.6, 2.5, false));
        assert_eq!(stab.optical_settings.read().strength, 0.3);
        // The last tick wins, as on the panel: reconstruction turns the other two off
        let stab = manager(false);
        run_queue_optical(1, &stab, &settings(serde_json::json!({ "translation": true, "correction": true, "reconstruction": true })), false, |_| Err("x".into()));
        let ui = *stab.optical_ui.read();
        assert!(!ui.translation_enabled && !ui.correction_enabled);
    }

    #[test]
    fn active_results_are_not_analyzed_again() {
        let stab = manager(false);
        let s = settings(serde_json::json!({ "translation": true }));
        stab.set_optical_correction_enabled(false);
        stab.set_translation_stabilization_enabled(true);
        install_translation(&stab);
        assert!(translation_active(&stab));
        let outcome = run_queue_optical(1, &stab, &s, false, |_| panic!("an applying result must not be analyzed again"));
        assert_eq!(outcome, QueueOpticalOutcome::Applied { analyzed: false });
        assert!(translation_active(&stab));
    }

    #[test]
    fn analysis_installs_the_missing_result() {
        let stab = manager(false);
        let outcome = run_queue_optical(1, &stab, &settings(serde_json::json!({ "translation": true })), false, |stab| {
            install_translation(stab);
            Ok(())
        });
        assert_eq!(outcome, QueueOpticalOutcome::Applied { analyzed: true });
        assert!(translation_active(&stab));
    }

    #[test]
    fn a_failed_translation_falls_back_to_the_basic_stabilization() {
        let stab = manager(false);
        let outcome = run_queue_optical(1, &stab, &settings(serde_json::json!({ "translation": true })), false, |_| Err("Not enough of the image could be tracked".into()));
        assert_eq!(outcome, QueueOpticalOutcome::Fallback { analyzed: true, items: vec![TRANSLATION], reason: "Not enough of the image could be tracked".into() });
        assert_eq!(outcome.notice(), "optical_fallback:translation:Not enough of the image could be tracked");
        assert!(!stab.optical_ui.read().translation_enabled, "the job is rendered without it");
    }

    #[test]
    fn correction_and_translation_fail_together() {
        let stab = manager(false);
        let s = settings(serde_json::json!({ "translation": true, "correction": true, "ignore_file_motion": false }));
        let outcome = run_queue_optical(1, &stab, &s, false, |_| Err("Video is not loaded".into()));
        assert_eq!(outcome, QueueOpticalOutcome::Fallback { analyzed: true, items: vec![CORRECTION, TRANSLATION], reason: "Video is not loaded".into() });
        let ui = *stab.optical_ui.read();
        assert!(!ui.correction_enabled && !ui.translation_enabled);
        assert!(!stab.gyro.read().ignores_file_motion());
    }

    #[test]
    fn a_failed_correction_restores_the_file_motion() {
        let stab = manager(false);
        let s = settings(serde_json::json!({ "correction": true, "ignore_file_motion": true }));
        let outcome = run_queue_optical(1, &stab, &s, false, |stab| {
            assert!(stab.gyro.read().ignores_file_motion(), "the analysis measures with the file motion set aside");
            Err("x".into())
        });
        assert_eq!(outcome, QueueOpticalOutcome::Fallback { analyzed: true, items: vec![CORRECTION], reason: "x".into() });
        assert!(!stab.gyro.read().ignores_file_motion());
        assert!(!stab.optical_ui.read().correction_enabled);
    }

    #[test]
    fn a_successful_analysis_without_a_translation_result_is_a_failure() {
        let stab = manager(false);
        let s = settings(serde_json::json!({ "translation": true, "correction": true }));
        let outcome = run_queue_optical(1, &stab, &s, false, |stab| { install_correction(stab); Ok(()) });
        assert_eq!(outcome, QueueOpticalOutcome::Fallback { analyzed: true, items: vec![TRANSLATION], reason: NO_RESULT.into() });
        assert!(correction_active(&stab), "the item that applies stays");
        assert!(!stab.optical_ui.read().translation_enabled);
    }

    #[test]
    fn a_cancelled_analysis_neither_falls_back_nor_tells() {
        let stab = manager(false);
        let outcome = run_queue_optical(1, &stab, &settings(serde_json::json!({ "translation": true })), false, |_| Err("Cancelled".into()));
        assert_eq!(outcome, QueueOpticalOutcome::Cancelled);
        assert_eq!(outcome.notice(), "");
        assert!(stab.optical_ui.read().translation_enabled, "a resumed render analyzes again");
    }

    #[test]
    fn items_that_need_motion_data_are_not_analyzed_without_it() {
        let stab = manager(false);
        stab.gyro.write().file_metadata.write().quaternions.clear();
        stab.recompute_gyro();
        let outcome = run_queue_optical(1, &stab, &settings(serde_json::json!({ "translation": true })), false, |_| panic!("nothing to analyze"));
        assert_eq!(outcome, QueueOpticalOutcome::Fallback { analyzed: false, items: vec![TRANSLATION], reason: NEEDS_MOTION.into() });
    }

    #[test]
    fn a_failed_reconstruction_skips_a_clip_that_needs_it() {
        let stab = manager(true);
        let s = settings(serde_json::json!({ "reconstruction": true }));
        let outcome = run_queue_optical(1, &stab, &s, true, |_| Err("NotConverged".into()));
        assert_eq!(outcome, QueueOpticalOutcome::Skip { reason: "NotConverged".into() });
        assert_eq!(outcome.notice(), "optical_skipped:reconstruction:NotConverged");
        // Without an error, but without a result either
        let stab = manager(true);
        let outcome = run_queue_optical(1, &stab, &s, true, |_| Ok(()));
        assert_eq!(outcome, QueueOpticalOutcome::Skip { reason: NO_RESULT.into() });
    }

    #[test]
    fn a_failed_reconstruction_never_skips_a_clip_that_does_not_need_it() {
        // Sony with IBIS/OIS data, Canon, stabilization off: the gate never blocked them
        let stab = manager(false);
        let s = settings(serde_json::json!({ "reconstruction": true }));
        let outcome = run_queue_optical(1, &stab, &s, false, |_| Err("NotConverged".into()));
        assert_eq!(outcome, QueueOpticalOutcome::Fallback { analyzed: true, items: vec![RECONSTRUCTION], reason: "NotConverged".into() });
        assert_eq!(outcome.notice(), "optical_fallback:reconstruction:NotConverged");
        assert!(!stab.optical_ui.read().stab_enabled, "processed as it was before the reconstruction");
    }

    #[test]
    fn a_working_reconstruction_lets_a_clip_that_needs_it_render() {
        let stab = manager(true);
        let outcome = run_queue_optical(1, &stab, &settings(serde_json::json!({ "reconstruction": true })), true, |stab| {
            install_reconstruction(stab);
            Ok(())
        });
        assert_eq!(outcome, QueueOpticalOutcome::Applied { analyzed: true });
        assert!(reconstruction_active(&stab));
    }

    #[test]
    fn a_clip_that_needs_compensation_is_skipped_when_the_settings_turn_its_reconstruction_off() {
        // A preview result came with the job, but the queue's settings untick the reconstruction
        let stab = manager(true);
        stab.set_stab_reconstruction_enabled(true);
        install_reconstruction(&stab);
        assert!(reconstruction_active(&stab));
        let outcome = run_queue_optical(1, &stab, &settings(serde_json::json!({ "translation": true })), true, |stab| {
            install_translation(stab);
            Ok(())
        });
        assert!(matches!(outcome, QueueOpticalOutcome::Skip { .. }), "{outcome:?}");
    }

    #[test]
    fn unticked_settings_turn_off_what_the_job_brought_along() {
        let stab = manager(false);
        stab.set_translation_stabilization_enabled(true);
        install_translation(&stab);
        assert!(translation_active(&stab));
        let outcome = run_queue_optical(1, &stab, &QueueOpticalSettings::default(), false, |_| panic!("nothing is ticked"));
        assert_eq!(outcome, QueueOpticalOutcome::Applied { analyzed: false });
        assert!(!translation_active(&stab));
    }

    // ---- stabilize-flow-optical-analysis ----

    /// What the predicates must leave as it is: the panel state, the strength, the motion data, the results, their context
    fn manager_state(stab: &StabilizationManager) -> String {
        let (ui, strength) = (*stab.optical_ui.read(), stab.optical_settings.read().strength);
        let gyro = stab.gyro.read();
        format!(
            "{ui:?} {strength} {} {:?} {:?} {:?} {} {} {:?}",
            gyro.ignores_file_motion(),
            gyro.optical_correction.as_ref().map(|c| (c.enabled, c.settings.strength)),
            gyro.optical_translation.as_ref().map(|t| (t.enabled, t.applies, t.settings)),
            gyro.optical_stab.as_ref().map(|s| (s.enabled, s.applies)),
            gyro.optical_context,
            gyro.optical_uncorrected_checksum,
            gyro.get_offsets()
        )
    }

    fn with_translation() -> StabilizationManager {
        let stab = manager(false);
        stab.set_optical_correction_enabled(false);
        stab.set_translation_stabilization_enabled(true);
        install_translation(&stab);
        stab
    }

    struct Fixture {
        name: &'static str,
        build: fn() -> StabilizationManager,
        settings: serde_json::Value,
        /// `needs_analysis`
        needs: bool,
        /// `run_queue_optical` calls the analysis
        analyzes: bool,
    }

    fn fixtures() -> Vec<Fixture> {
        use serde_json::json;
        vec![
            Fixture { name: "no result", build: || manager(false), settings: json!({ "translation": true }), needs: true, analyzes: true },
            Fixture { name: "result applies", build: with_translation, settings: json!({ "translation": true }), needs: false, analyzes: false },
            Fixture {
                name: "result applies, switched off on the manager",
                build: || { let stab = with_translation(); stab.set_translation_stabilization_enabled(false); stab },
                settings: json!({ "translation": true }), needs: false, analyzes: false,
            },
            Fixture {
                name: "a sync point moved after the analysis",
                build: || { let stab = with_translation(); stab.gyro.write().set_offset(0, 25.0); stab },
                settings: json!({ "translation": true }), needs: true, analyzes: true,
            },
            Fixture {
                name: "correction fitted with its strength",
                build: || { let stab = manager(false); stab.set_optical_correction_enabled(true); install_correction(&stab); stab },
                settings: json!({ "correction": true }), needs: false, analyzes: false,
            },
            Fixture {
                name: "correction fitted with another strength",
                build: || { let stab = manager(false); stab.set_optical_correction_enabled(true); install_correction(&stab); stab },
                settings: json!({ "correction": true, "strength": 0.9 }), needs: true, analyzes: true,
            },
            Fixture {
                name: "reconstruction wins over the other two, and applies",
                build: || { let stab = manager(false); stab.set_stab_reconstruction_enabled(true); install_reconstruction(&stab); stab },
                settings: json!({ "translation": true, "correction": true, "reconstruction": true }), needs: false, analyzes: false,
            },
            Fixture {
                name: "reconstruction wins over a translation that applies",
                build: with_translation,
                settings: json!({ "translation": true, "reconstruction": true }), needs: true, analyzes: true,
            },
            Fixture {
                name: "needs motion data the clip lacks",
                build: || { let stab = manager(false); stab.gyro.write().file_metadata.write().quaternions.clear(); stab.recompute_gyro(); stab },
                settings: json!({ "translation": true }), needs: true, analyzes: false,
            },
            Fixture { name: "nothing ticked", build: with_translation, settings: json!({}), needs: false, analyzes: false },
            Fixture {
                name: "the correction sets the motion data aside",
                build: || manager(false),
                settings: json!({ "correction": true, "ignore_file_motion": true }), needs: true, analyzes: true,
            },
        ]
    }

    #[test]
    fn needs_analysis_agrees_with_the_analysis_step() {
        for f in fixtures() {
            let s = settings(f.settings.clone());
            let stab = (f.build)();
            let before = manager_state(&stab);
            assert_eq!(needs_analysis(&stab, &s), f.needs, "{}", f.name);
            let differ = settings_differ(&stab, &s);
            stabilize_pass_needed(&stab, &s);
            assert_eq!(manager_state(&stab), before, "{}: the predicates leave the manager as it is", f.name);
            assert_eq!(apply_settings(1, &stab, &s), differ, "{}: settings_differ is what applying them changes", f.name);

            let stab = (f.build)();
            let calls = Cell::new(0);
            let outcome = run_queue_optical(1, &stab, &s, false, |_| { calls.set(calls.get() + 1); Err("x".into()) });
            assert_eq!(calls.get() > 0, f.analyzes, "{}: {outcome:?}", f.name);
            if !needs_analysis(&(f.build)(), &s) {
                assert_eq!(calls.get(), 0, "{}: nothing to analyze", f.name);
                assert_eq!(outcome, QueueOpticalOutcome::Applied { analyzed: false }, "{}", f.name);
            }
        }
    }

    #[test]
    fn a_stabilize_pass_is_needed_for_a_missing_result_or_other_settings() {
        let translation = settings(serde_json::json!({ "translation": true }));
        assert!(stabilize_pass_needed(&manager(false), &translation), "no result yet");

        // Applied once (what an earlier pass leaves): the same settings need nothing
        let stab = with_translation();
        apply_settings(1, &stab, &translation);
        assert!(!needs_analysis(&stab, &translation) && !settings_differ(&stab, &translation));
        assert!(!stabilize_pass_needed(&stab, &translation), "the result applies and the settings are the same");

        // Another smoothness: no analysis, but the project must carry the new setting
        let smoother = settings(serde_json::json!({ "translation": true, "translation_smoothness": 2.5 }));
        assert!(!needs_analysis(&stab, &smoother));
        assert!(settings_differ(&stab, &smoother));
        assert!(stabilize_pass_needed(&stab, &smoother));

        // Nothing ticked: never, even though applying the settings would switch the translation off
        let none = QueueOpticalSettings::default();
        assert!(settings_differ(&stab, &none));
        assert!(!stabilize_pass_needed(&stab, &none));

        // A value the setter clamps counts as the clamped one
        apply_settings(1, &stab, &settings(serde_json::json!({ "translation": true, "translation_smoothness": 0.1, "strength": 1.0 })));
        let below = settings(serde_json::json!({ "translation": true, "translation_smoothness": 0.01, "strength": 3.0 }));
        assert!(!settings_differ(&stab, &below));
    }

    #[test]
    fn a_correction_fitted_with_another_strength_is_analyzed_again() {
        let stab = manager(false);
        stab.set_optical_correction_enabled(true);
        install_correction(&stab);
        assert!(correction_active(&stab));
        let s = settings(serde_json::json!({ "correction": true, "strength": 0.9 }));
        let calls = Cell::new(0);
        let outcome = run_queue_optical(1, &stab, &s, false, |stab| { calls.set(calls.get() + 1); install_correction(stab); Ok(()) });
        assert_eq!(calls.get(), 1, "no kept measurements to refit: only an analysis applies the new strength");
        assert_eq!(outcome, QueueOpticalOutcome::Applied { analyzed: true });
        assert_eq!(stab.gyro.read().optical_correction.as_ref().unwrap().settings.strength, 0.9);
    }
}
