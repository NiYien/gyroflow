// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

//! Which QML-facing settings keys are allowed to touch persistent storage.
//!
//! Four incidents shared one shape: a persisted key the Simple-mode user can
//! neither see nor reach kept driving behaviour behind their back. Portrait
//! `preserved*` output sizes squashed landscape footage into a 9:16 strip; a
//! stale `simpleAiSync` drove batch sync; sync overlays could not be switched
//! off; and a `smoothingMethod` of "Fixed camera" collapsed the adaptive zoom
//! until the preview went black. Each was patched one key at a time.
//!
//! This module inverts the default: a key reaches the disk only if it is on the
//! allow list, which is derived from what Simple mode actually exposes. Denied
//! keys behave exactly as they do on a fresh install — `value()` hands back the
//! caller's own default, `setValue()` is dropped, and whatever sits on disk is
//! frozen rather than deleted.
//!
//! The gate is unconditional; it never asks which mode is active. That is sound
//! because startup is always Simple (`App.qml` assigns `isSimpleMode = true`
//! unconditionally) and the Full-mode entries are only visible when
//! `GYROFLOW_NIYIEN_FULL_MODE=1`, which disables the gate outright. Keeping the
//! gate mode-blind removes the startup-ordering hazard that defeated the three
//! earlier fixes: an `asynchronous` ItemLoader restoring a value after the
//! mode-dependent reset had already run.

use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};

/// Keys Simple mode genuinely exposes, and which therefore persist.
///
/// Every entry is backed by a control the user can actually reach; see the
/// change's design notes for the per-key justification.
const ALLOW_EXACT: &[&str] = &[
    // Simple settings card: language and theme combo boxes are visible.
    "lang",
    "theme",
    // No Simple-mode entry, allowed anyway: pure display scaling that touches no
    // frame geometry, and resetting it hurts high-DPI users for nothing.
    "uiScaling",
    // Onboarding tutorial must not replay on every launch.
    "niyien_tutorial_seen_v1",
    // SimpleDevice card: timezone picker.
    "niyienTimezoneKey",
    "niyienTimezoneLabel",
    "niyienTimezoneOffsetMinutes",
    "niyienTimezoneRegionX",
    "niyienTimezoneRegionY",
    // MountingPresetSelector card.
    "mountingMode",
    "mountingPosition",
    "mountingRotation",
    "mountingCustomPitch",
    "mountingCustomRoll",
    "mountingCustomYaw",
    // SimpleExport: the output-location combo and its fixed-path field are the
    // only export controls not hidden in Simple mode.
    "queueOutputMode",
    "queueFixedOutputPath",
    // Window geometry and panel splitters: user-dragged, no bearing on output.
    "windowX",
    "windowY",
    "windowWidth",
    "windowHeight",
    "visibility",
    "leftPanelSize",
    "rightPanelSize",
    "bottomPanelSize",
    "bottomPanelSize-full",
    // Player volume.
    "volume",
];

/// Allowed key families. Matched before the deny families, so a narrower allow
/// prefix can carve an exception out of a broader denial.
const ALLOW_PREFIX: &[&str] = &[
    // "Do not show again" is an explicit user choice.
    "dontShowAgain-",
    // Last-used directory for file dialogs. Verified not to feed the output
    // path: that comes from the source folder and `preservedOutputPath`, both
    // handled elsewhere.
    "folder-",
    // The Simple-mode smoothness slider does not persist itself; it rides on the
    // Full-mode panel's per-algorithm key. Only algorithm 1 (DefaultAlgo) is
    // allowed through — `smoothing-3-*` is Fixed camera, the black-preview bug.
    "smoothing-1-",
];

/// Keys deliberately kept off disk. Runtime behaviour does not consult this list
/// (anything not allowed is denied), but every key found in QML must appear in
/// one of the two tables so that adding a setting forces an explicit decision.
const DENY_EXACT: &[&str] = &[
    // Stabilization panel — `smoothingMethod` is the black-preview incident.
    "smoothingMethod",
    "croppingMode",
    "adaptiveZoom",
    "correctionAmount",
    "useGravityVectors",
    "hlIntegrationMethod",
    "videoSpeedAffectsSmoothing",
    "videoSpeedAffectsZooming",
    "videoSpeedAffectsZoomingLimit",
    "zoomingMethod",
    "maxZoom",
    "maxZoomIterations",
    // Synchronization panel.
    "initialOffset",
    "syncSearchSize",
    "maxSyncPoints",
    "timePerSyncpoint",
    "sync_lpf",
    "checkNegativeInitialOffset",
    "experimentalAutoSyncPoints",
    // Export panel. `preserveOutputSettings` / `preserveOutputPath` formalise
    // the ad-hoc guards added when portrait output sizes leaked in.
    "defaultCodec",
    "exportAudio",
    "keyframeDistance",
    "preserveOtherTracks",
    "padWithBlack",
    "exportTrimsSeparately",
    "useVulkanEncoder",
    "useD3D12Encoder",
    "metadataComment",
    "audioCodec",
    "interpolationMethod",
    "preserveOutputSettings",
    "preserveOutputPath",
    // Export-adjacent loose keys.
    "preservedWidth",
    "preservedHeight",
    "preservedBitrate",
    "preservedOutputPath",
    "outputSizePresets",
    "exportMode",
    // Advanced panel.
    "previewPipeline",
    "renderBackground",
    "safeAreaGuide",
    "gpudecode",
    "backgroundMode",
    "marginPixels",
    "featherPixels",
    "defaultSuffix",
    "playSounds",
    "r3dConvertFormat",
    "r3dColorMode",
    "r3dGammaCurve",
    "r3dColorSpace",
    "r3dRedlineParams",
    // Device selection: denial means automatic selection, which is the safer
    // default and the one a fresh install gets.
    "processingDevice",
    "processingDeviceIndex",
    "renderingDevice",
    // Preview resolution. Denial returns -1, which lets VideoArea's existing
    // fallback pick 1080p — the same value the field produced before.
    "previewResolution",
    // Lens calibration.
    "calib_maxPoints",
    "calib_everyNthFrame",
    "calib_iterations",
    "calib_maxSharpness",
    "calibratedBy",
    "calibPanelSize",
    "CSVExportSelection",
    "lensProfileFavorites",
    // Timeline and preview aids.
    "timelineChart",
    "restrictTrimRange",
    "gridLines",
    "stabOverviewSplit",
    // Queue. Simple mode already forces its own values for the first two, so
    // denial simply stops the stored copies from being consulted.
    "parallelRenders_v2",
    "defaultOverwriteAction",
    "showQueueWhenAdding",
    "imageSequenceFps",
    // Retired feature: AI sync is disabled, and a stale preference here once
    // drove batch sync anyway.
    "simpleAiSync",
];

/// Denied key families.
const DENY_PREFIX: &[&str] = &[
    "encoderOptions-",
    "exportGpu-",
    "rated-profile-",
    // Broad denial; `smoothing-1-` above carves out the one allowed algorithm.
    "smoothing-",
];

/// Families folded into a single entry in the diagnostic summary so one noisy
/// family cannot bury the rest of the report.
const SUMMARY_FOLD_PREFIX: &[&str] = &[
    "encoderOptions-",
    "exportGpu-",
    "rated-profile-",
    "smoothing-",
];

/// Whether the whole gate is switched off.
///
/// `GYROFLOW_NIYIEN_FULL_MODE=1` is already documented in `controller.rs` as a
/// debug/dev escape hatch, and it is the only thing that reveals the Full-mode
/// entries at all. Disabling the gate with it keeps that hatch able to observe
/// and reproduce a user's real on-disk settings.
pub fn gate_disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_NIYIEN_FULL_MODE").ok();
        let disabled = parse_gate_disabled(raw.as_deref());
        ::log::info!(
            target: "lifecycle",
            "settings gate resolved: enabled={} source={}",
            !disabled,
            if disabled { "env" } else { "default" }
        );
        disabled
    })
}

/// Split out from the `OnceLock` so it stays testable: the cached value cannot be
/// re-resolved once any test has touched it.
///
/// Deliberately exact-matches `1` rather than accepting the usual
/// `on/true/yes` spellings, because this mirrors `Controller::full_mode_enabled`
/// — the two must agree or the Full-mode entries would appear while the gate
/// stayed on (or the reverse).
fn parse_gate_disabled(raw: Option<&str>) -> bool {
    raw == Some("1")
}

/// Pure table lookup: may this key reach persistent storage?
///
/// Allow rules win over deny rules, which is what lets `smoothing-1-` survive
/// the blanket `smoothing-` denial.
pub fn is_persisted(key: &str) -> bool {
    if ALLOW_EXACT.contains(&key) {
        return true;
    }
    ALLOW_PREFIX.iter().any(|p| key.starts_with(p))
}

/// Whether the key appears in either table. Runtime never needs this; the guard
/// test does, to prove no key slipped in without a decision being taken.
#[cfg(test)]
pub fn is_classified(key: &str) -> bool {
    if is_persisted(key) || DENY_EXACT.contains(&key) {
        return true;
    }
    DENY_PREFIX.iter().any(|p| key.starts_with(p))
}

fn denied_keys() -> &'static Mutex<BTreeSet<String>> {
    static KEYS: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    KEYS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// Collapse a denied key to the label used in the summary line.
fn summary_label(key: &str) -> String {
    for prefix in SUMMARY_FOLD_PREFIX {
        if key.starts_with(prefix) {
            return format!("{prefix}*");
        }
    }
    key.to_string()
}

/// Record a denial and make sure the one-shot summary gets emitted.
///
/// The summary is the leak detector: a whitelist that denies by default breaks a
/// feature silently when a key is missed, so the denied set has to be visible in
/// an ordinary user log without needing a reproduction.
fn note_denied(key: &str) {
    if let Ok(mut set) = denied_keys().lock() {
        set.insert(summary_label(key));
    }
    static SUMMARY: OnceLock<()> = OnceLock::new();
    SUMMARY.get_or_init(|| {
        // Startup reads are spread over asynchronous panel loading, so settle
        // before reporting rather than emitting a line per key.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let keys = denied_keys()
                .lock()
                .map(|s| s.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            ::log::info!(
                target: "lifecycle",
                "simple_mode_settings_gate: denied={} keys=[{}]",
                keys.len(),
                keys.join(", ")
            );
        });
    });
}

/// The call the `Settings` QObject uses. Denials are recorded on the way out.
pub fn allows(key: &str) -> bool {
    if gate_disabled() || is_persisted(key) {
        return true;
    }
    note_denied(key);
    false
}

/// Resolve the gate once and hand the verdict to `gyroflow-core`, which needs it
/// for the project-import path.
///
/// Called from the GUI entry point only, and only after the CLI has had its
/// chance to take over: a headless render asked to apply a project must apply it
/// verbatim, exactly as the NLE plugins do.
pub fn init() {
    gyroflow_core::settings::set_project_import_gate(!gate_disabled());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn allowed_keys_persist() {
        for key in [
            "lang",
            "theme",
            "uiScaling",
            "volume",
            "mountingMode",
            "queueOutputMode",
            "queueFixedOutputPath",
            "niyien_tutorial_seen_v1",
            "bottomPanelSize",
            "bottomPanelSize-full",
        ] {
            assert!(is_persisted(key), "expected {key} to persist");
        }
    }

    #[test]
    fn denied_keys_do_not_persist() {
        for key in [
            "smoothingMethod",
            "maxZoom",
            "croppingMode",
            "preserveOutputSettings",
            "preservedWidth",
            "simpleAiSync",
            "previewResolution",
            "defaultOverwriteAction",
            "gpudecode",
            "processingDevice",
        ] {
            assert!(!is_persisted(key), "expected {key} to be denied");
        }
    }

    #[test]
    fn only_default_algorithm_smoothing_params_persist() {
        assert!(is_persisted("smoothing-1-smoothness"));
        assert!(is_persisted("smoothing-1-per_axis"));
        // Fixed camera is algorithm 3 — the incident that motivated the gate.
        assert!(!is_persisted("smoothing-3-roll"));
        assert!(!is_persisted("smoothing-3-pitch"));
        assert!(!is_persisted("smoothing-0-anything"));
        assert!(!is_persisted("smoothing-2-time_constant"));
    }

    #[test]
    fn allowed_families_match_by_prefix() {
        assert!(is_persisted("dontShowAgain-someDialog"));
        assert!(is_persisted("folder-video"));
        assert!(is_persisted("folder-lensprofile"));
        assert!(!is_persisted("encoderOptions-3"));
        assert!(!is_persisted("exportGpu-0"));
        assert!(!is_persisted("rated-profile-abc123"));
    }

    #[test]
    fn unknown_keys_are_denied_but_flagged_as_unclassified() {
        assert!(!is_persisted("someKeyNobodyClassifiedYet"));
        assert!(!is_classified("someKeyNobodyClassifiedYet"));
    }

    #[test]
    fn gate_is_only_disabled_by_the_exact_full_mode_value() {
        assert!(parse_gate_disabled(Some("1")));
        assert!(!parse_gate_disabled(None));
        assert!(!parse_gate_disabled(Some("")));
        assert!(!parse_gate_disabled(Some("0")));
        // Must stay in lockstep with Controller::full_mode_enabled, which also
        // only accepts "1". Accepting more spellings here would let the gate turn
        // off while the Full-mode entries stayed hidden.
        assert!(!parse_gate_disabled(Some("true")));
        assert!(!parse_gate_disabled(Some("on")));
        assert!(!parse_gate_disabled(Some("yes")));
    }

    #[test]
    fn every_gated_project_field_maps_to_a_denied_key() {
        // The project-import gate lives in gyroflow-core, which cannot see this
        // table. Gating a field whose key is actually reachable in Simple mode
        // would silently discard the user's own choice on every project load, so
        // the two lists have to be checked against each other.
        for (field, key) in gyroflow_core::settings::GATED_PROJECT_STABILIZATION_FIELDS {
            assert!(
                is_classified(key),
                "gated project field {field:?} maps to unclassified key {key:?}"
            );
            assert!(
                !is_persisted(key),
                "project field {field:?} is gated on import, but its key {key:?} is \
                 user-reachable in Simple mode — one of the two is wrong"
            );
        }
    }

    #[test]
    fn summary_folds_noisy_families() {
        assert_eq!(summary_label("smoothing-3-roll"), "smoothing-*");
        assert_eq!(summary_label("encoderOptions-2"), "encoderOptions-*");
        assert_eq!(summary_label("maxZoom"), "maxZoom");
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn collect_qml(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_qml(&path, out);
            } else if path.extension().map(|e| e == "qml").unwrap_or(false) {
                out.push(path);
            }
        }
    }

    /// Literal keys passed to `settings.value(` / `settings.setValue(`.
    ///
    /// Concatenated keys such as `"smoothing-" + index + "-" + name` yield their
    /// literal prefix, which is exactly the granularity the tables express.
    fn extract_call_keys(source: &str, out: &mut Vec<String>) {
        for call in ["settings.value(\"", "settings.setValue(\""] {
            let mut rest = source;
            while let Some(pos) = rest.find(call) {
                rest = &rest[pos + call.len()..];
                if let Some(end) = rest.find('"') {
                    let key = &rest[..end];
                    if !key.is_empty() {
                        out.push(key.to_string());
                    }
                }
            }
        }
    }

    /// Property names inside a `sett` block, which `settings.init` persists
    /// wholesale by iterating the object's meta properties.
    fn extract_sett_keys(source: &str, out: &mut Vec<String>) {
        let Some(start) = source.find("id: sett;") else {
            return;
        };
        let block = &source[start..];
        let end = block.find("settings.init").unwrap_or(block.len());
        for line in block[..end].lines() {
            let line = line.trim();
            if line.starts_with("//") || !line.starts_with("property ") {
                continue;
            }
            let Some(colon) = line.find(':') else { continue };
            let Some(name) = line[..colon].split_whitespace().last() else {
                continue;
            };
            out.push(name.to_string());
        }
    }

    #[test]
    fn every_qml_settings_key_is_classified() {
        let ui_root = repo_root().join("src").join("ui");
        let mut files = Vec::new();
        collect_qml(&ui_root, &mut files);
        assert!(
            files.len() > 10,
            "found only {} qml files under {ui_root:?} — the scan root is wrong",
            files.len()
        );

        let mut unclassified = Vec::new();
        let mut seen = BTreeSet::new();
        for file in files {
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            let mut keys = Vec::new();
            extract_call_keys(&source, &mut keys);
            extract_sett_keys(&source, &mut keys);
            seen.extend(keys.iter().cloned());
            for key in keys {
                if !is_classified(&key) {
                    let shown = file
                        .strip_prefix(&ui_root)
                        .unwrap_or(&file)
                        .display()
                        .to_string();
                    unclassified.push(format!("  {key}  (src/ui/{shown})"));
                }
            }
        }
        // Without this the test passes vacuously if the extractor silently stops
        // matching — a green run would then read as "everything is classified"
        // while nothing was actually examined.
        assert!(
            seen.len() >= 90,
            "extractor found only {} distinct keys across the QML tree; it is \
             probably broken rather than the tree having shrunk",
            seen.len()
        );

        unclassified.sort();
        unclassified.dedup();

        assert!(
            unclassified.is_empty(),
            "these settings keys are in neither the allow nor the deny table:\n{}\n\n\
             Every persisted key must be classified explicitly. Add it to \
             ALLOW_EXACT/ALLOW_PREFIX only if Simple mode exposes a control for \
             it; otherwise add it to DENY_EXACT/DENY_PREFIX.",
            unclassified.join("\n")
        );
    }

    #[test]
    fn key_extraction_handles_the_shapes_used_in_qml() {
        let mut keys = Vec::new();
        extract_call_keys(
            r#"
            settings.value("plainKey", 0);
            settings.setValue("anotherKey", v);
            settings.value("prefixed-" + identifier, 0);
            settings.value("bottomPanelSize" + (root.fullScreen? "-full" : ""), 0);
            "#,
            &mut keys,
        );
        assert_eq!(
            keys,
            vec![
                "plainKey",
                "prefixed-",
                "bottomPanelSize",
                "anotherKey"
            ]
        );

        let mut sett_keys = Vec::new();
        extract_sett_keys(
            r#"
            Item {
                id: sett;
                property alias someAlias: someControl.currentIndex;
                // property alias commentedOut: nope.value;
                property string someString: "x";
                Component.onCompleted: settings.init(sett);
            }
            property alias notInBlock: other.value;
            "#,
            &mut sett_keys,
        );
        assert_eq!(sett_keys, vec!["someAlias", "someString"]);
    }
}
