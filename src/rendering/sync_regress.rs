// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! Headless regression harness for the sync methods (optical motion sync spec §9.2).
//!
//! `GYROFLOW_SYNC_REGRESS=<corpus.json>` makes `cli::run` call `run` before anything else. For every clip of the
//! corpus the existing chain (`offset_method` 2) and the optical motion method (3) run on the same sync points and
//! parameters through the real `AutosyncProcess`, each on a freshly cleared `PoseEstimator`.
//!
//! Decoding and feeding mirror `Controller::start_autosync`'s `try_run`, which stays authoritative: 1080 px processing
//! height (decoder `scale` option and converter scaling), GRAY8 (NV12 for the NeuFlow `of_method`s), every
//! `every_nth_frame`-th frame, the DNG tone curve when the file has one, the process' own ranges, then
//! `finished_feeding_frames` and, when `pending_probe_ranges` asks for it, the lazy-probe second pass (its time
//! counts). Image-sequence decoder options are not mirrored: the corpus holds video files only.
//!
//! `GYROFLOW_SYNC_REGRESS_DECODE` picks the decoder: `sw` (default) decodes in software only; `gpu` decodes as the
//! controller does with `gpudecode` on: the GPU codec blocklist is cleared at every method run, the codec signature
//! is probed, a blocklisted codec goes straight to software, otherwise GPU first and, when that pass fails with
//! `GPUDecodingFailed`, software on the same `AutosyncProcess` (the first pass also blocklists the codec). Which
//! decoder delivered the frames is taken from the decoder's own `Selected HW backend` log line.
//!
//! Timing: a method's total runs from before `AutosyncProcess::from_manager` to the end of the result callback and is
//! split evenly across its windows. The decode wall time (start_decoder_only calls) and the time spent inside
//! `feed_frame` are recorded separately, so a cold file cache on the first method (2) stays visible.
//!
//! Output in the corpus' `out_dir`: `rows.csv` (`Row` plus `err_ms`) and `summary.md` (both methods side by side
//! per window, the optical method's G / near / stage timings parsed from its `[optical]` log lines, and the five
//! acceptance gates of spec §9.3). The log lines are taken in-process from the logger's ring buffer, after a marker
//! line written at the start of every method run.
//!
//! Environment: `GYROFLOW_SYNC_REGRESS_ONLY=<name,name>` runs only those clips; `GYROFLOW_SYNC_REGRESS_GATE=1`
//! makes the exit code reflect the gates; `GYROFLOW_SYNC_REGRESS_DECODE=sw|gpu` as above.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use ffmpeg_next::format::Pixel;
use gyroflow_core::StabilizationManager;
use gyroflow_core::synchronization::{AutosyncProcess, AutosyncResult, SyncParams};
use parking_lot::Mutex;

use super::gpu_codec_blocklist::CodecSignature;
use super::{FFmpegError, VideoProcessor};

/// The existing chain (rs-sync, fusion, posterior, arbitration)
const BASELINE: usize = 2;
/// The optical motion method
const OPTICAL: usize = 3;
const METHODS: [usize; 2] = [BASELINE, OPTICAL];
/// Below this confidence the controller and the render queue drop a sync point
const CONF_GATE: f64 = 0.4;
/// The controller's default processing resolution
const PROC_HEIGHT: i32 = 1080;
const MODE: &str = "synchronize";
const DEFAULT_TOLERANCE_MS: f64 = 3.0;

/// How the frames are decoded (`GYROFLOW_SYNC_REGRESS_DECODE`)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecodeMode {
    /// Software only (the default)
    Software,
    /// As the controller with `gpudecode` on: GPU first, software when GPU decoding fails
    GpuFirst,
}

impl DecodeMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Software => "sw",
            Self::GpuFirst => "gpu",
        }
    }
}

fn resolve_decode_mode() -> Result<DecodeMode, String> {
    let raw = std::env::var("GYROFLOW_SYNC_REGRESS_DECODE");
    let (mode, source) = match raw.as_deref().map(str::trim) {
        Err(_) | Ok("") => (DecodeMode::Software, "default"),
        Ok(v) => match v.to_ascii_lowercase().as_str() {
            "sw" => (DecodeMode::Software, "env"),
            "gpu" => (DecodeMode::GpuFirst, "env"),
            _ => return Err(format!("GYROFLOW_SYNC_REGRESS_DECODE must be sw or gpu, got {v:?}")),
        },
    };
    log::info!(target: "lifecycle", "GYROFLOW_SYNC_REGRESS_DECODE resolved value={} source={source}", mode.as_str());
    Ok(mode)
}

/// One window of one clip, synchronized by one method
#[derive(Clone, Debug)]
pub struct Row {
    pub clip: String,
    pub window: usize,
    pub method: usize,
    pub gate: bool,
    pub truth_ms: Option<f64>,
    pub tol_ms: f64,
    /// NaN when the method delivered no row for the window
    pub offset_ms: f64,
    pub conf: f64,
    pub cost: f64,
    /// The method's total time for the clip divided by the window count
    pub total_ms: f64,
}

impl Row {
    fn err_ms(&self) -> Option<f64> {
        self.truth_ms.map(|t| self.offset_ms - t).filter(|e| e.is_finite())
    }
    fn id(&self) -> String {
        format!("{} w{}", self.clip, self.window)
    }
}

/// The acceptance gates of spec §9.3 over the gated rows with a known truth
#[derive(Debug, Default)]
pub struct GateReport {
    /// Gate 1: optical rows with conf >= 0.4 outside the tolerance
    pub false_accepts: Vec<String>,
    /// Gate 2: optical_correct >= baseline_correct
    pub optical_correct: usize,
    pub baseline_correct: usize,
    /// Gate 3: windows the baseline got right and the optical method did not
    pub regressions: Vec<String>,
    /// Gate 4: known fixes the optical method did not get right
    pub known_fixes_missing: Vec<String>,
    /// Gate 5: windows where the optical method took longer than the baseline
    pub slower: Vec<String>,
}

impl GateReport {
    pub fn passed(&self) -> bool {
        self.false_accepts.is_empty()
            && self.optical_correct >= self.baseline_correct
            && self.regressions.is_empty()
            && self.known_fixes_missing.is_empty()
            && self.slower.is_empty()
    }
}

/// Truth known, conf >= 0.4 and the offset within the tolerance
pub fn is_correct(r: &Row) -> bool {
    r.truth_ms.is_some_and(|t| r.conf >= CONF_GATE && (r.offset_ms - t).abs() <= r.tol_ms)
}

fn describe(r: &Row) -> String {
    format!(
        "{} m{} (offset {} conf {:.3} err {})",
        r.id(),
        r.method,
        cell(r.offset_ms, 2),
        r.conf,
        r.err_ms().map_or("-".into(), |e| format!("{e:+.1}ms"))
    )
}

/// Only rows with `gate == true` and a known truth take part. A window is matched across methods by (clip, window).
pub fn evaluate_gates(rows: &[Row], known_fixes: &[&str]) -> GateReport {
    let gated: Vec<&Row> = rows.iter().filter(|r| r.gate && r.truth_ms.is_some()).collect();
    let find = |clip: &str, window: usize, method: usize| {
        gated.iter().copied().find(|r| r.clip == clip && r.window == window && r.method == method)
    };
    let mut report = GateReport::default();
    for o in gated.iter().filter(|r| r.method == OPTICAL) {
        if is_correct(o) {
            report.optical_correct += 1;
        } else if o.conf >= CONF_GATE {
            report.false_accepts.push(describe(o));
        }
    }
    for b in gated.iter().filter(|r| r.method == BASELINE) {
        let o = find(&b.clip, b.window, OPTICAL);
        if is_correct(b) {
            report.baseline_correct += 1;
            if !o.is_some_and(is_correct) {
                report.regressions.push(format!("{}: {}", describe(b), o.map_or("no optical row".into(), describe)));
            }
        }
        if let Some(o) = o {
            if o.total_ms > b.total_ms {
                report.slower.push(format!("{} ({:.0} ms > {:.0} ms)", b.id(), o.total_ms, b.total_ms));
            }
        }
    }
    for name in known_fixes {
        let optical: Vec<&Row> = gated.iter().copied().filter(|r| r.clip == *name && r.method == OPTICAL).collect();
        if optical.is_empty() || !optical.iter().all(|r| is_correct(r)) {
            report.known_fixes_missing.push(name.to_string());
        }
    }
    report
}

// ---------------------------------------------------------------------------------------------------------------------
// Corpus

#[derive(serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Corpus {
    out_dir: String,
    #[serde(default)]
    known_fixes: Vec<String>,
    clips: Vec<Clip>,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct Clip {
    name: String,
    project: String,
    video: String,
    #[serde(default)]
    truth: Option<f64>,
    #[serde(default = "default_tolerance")]
    tolerance: f64,
    #[serde(default = "default_gate")]
    gate: bool,
    /// Sync point positions, video ms
    sync_points_ms: Vec<f64>,
    /// `SyncParams` fields to override, in the units `AutosyncProcess` takes (ms)
    #[serde(default)]
    params: Option<serde_json::Map<String, serde_json::Value>>,
}

fn default_tolerance() -> f64 {
    DEFAULT_TOLERANCE_MS
}
fn default_gate() -> bool {
    true
}

impl Corpus {
    fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
        let corpus: Corpus = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
        if corpus.clips.is_empty() {
            return Err("no clips".into());
        }
        for (i, clip) in corpus.clips.iter().enumerate() {
            if corpus.clips[..i].iter().any(|c| c.name == clip.name) {
                return Err(format!("duplicate clip name {}", clip.name));
            }
            if clip.sync_points_ms.is_empty() || clip.sync_points_ms.iter().any(|x| !x.is_finite() || *x < 0.0) {
                return Err(format!("{}: sync_points_ms must be a non-empty list of positions >= 0", clip.name));
            }
            if !(clip.tolerance > 0.0) {
                return Err(format!("{}: tolerance must be > 0", clip.name));
            }
            for file in [&clip.project, &clip.video] {
                if !Path::new(file).is_file() {
                    return Err(format!("{}: file not found: {file}", clip.name));
                }
            }
            for method in METHODS {
                sync_params_for(clip, method).map_err(|e| format!("{}: {e}", clip.name))?;
            }
        }
        for name in &corpus.known_fixes {
            if !corpus.clips.iter().any(|c| &c.name == name) {
                return Err(format!("known fix {name} is not a clip of the corpus"));
            }
        }
        Ok(corpus)
    }
}

/// The parameters of one method run: product defaults (1.5 s windows, ±5 s search, initial offset 0), the clip's
/// overrides, then the controller's clamping. `normalize_for_mode` is applied by the caller.
fn sync_params_for(clip: &Clip, offset_method: usize) -> Result<SyncParams, String> {
    let base = SyncParams {
        initial_offset: 0.0,
        search_size: 5000.0,
        time_per_syncpoint: 1500.0,
        every_nth_frame: 1,
        of_method: 2,
        offset_method,
        max_sync_points: clip.sync_points_ms.len(),
        ..Default::default()
    };
    let mut params = match &clip.params {
        None => base,
        Some(overrides) => {
            let mut value = serde_json::to_value(&base).map_err(|e| e.to_string())?;
            let obj = value.as_object_mut().ok_or("SyncParams does not serialize to an object")?;
            for (key, v) in overrides {
                if key == "offset_method" {
                    return Err("params may not override offset_method (both methods always run)".into());
                }
                if !obj.contains_key(key) {
                    return Err(format!("params: unknown SyncParams field `{key}`"));
                }
                obj.insert(key.clone(), v.clone());
            }
            serde_json::from_value(value).map_err(|e| format!("params: {e}"))?
        }
    };
    params.every_nth_frame = params.every_nth_frame.max(1);
    Ok(params)
}

// ---------------------------------------------------------------------------------------------------------------------
// Running

/// What the import tells about a clip
#[derive(Clone, Debug, Default)]
struct ClipInfo {
    width: usize,
    height: usize,
    fps: f64,
    duration_ms: f64,
    fps_scale: Option<f64>,
    quats: usize,
}

#[derive(Clone, Debug, Default)]
struct DecodeTiming {
    /// Wall time of the `start_decoder_only` calls, feeding included
    wall_ms: f64,
    /// Time inside `AutosyncProcess::feed_frame`
    feed_ms: f64,
    frames: usize,
}

/// One method on one clip
#[derive(Clone, Debug)]
struct MethodRun {
    method: usize,
    /// Window ranges of the process, video ms
    ranges_ms: Vec<(f64, f64)>,
    /// The rows of the result callback; None when it never came
    result: Option<Vec<(f64, f64, f64, f64)>>,
    total_ms: f64,
    timing: DecodeTiming,
    /// The lazy probe was decoded (existing chain)
    probe: bool,
    /// Per decode pass, which decoder delivered the frames
    decoders: Vec<String>,
    errors: Vec<String>,
    /// `[optical]` / `[posterior]` log lines of the run, from the tag on
    log: Vec<String>,
}

struct ClipResult {
    clip: Clip,
    info: Option<ClipInfo>,
    runs: Vec<MethodRun>,
    rows: Vec<Row>,
    error: Option<String>,
}

pub fn run(corpus_path: &str) -> i32 {
    let decode = match resolve_decode_mode() {
        Ok(m) => m,
        Err(e) => {
            println!("[regress] {e}");
            return 2;
        }
    };
    let corpus = match Corpus::load(corpus_path) {
        Ok(c) => c,
        Err(e) => {
            println!("[regress] bad corpus {corpus_path}: {e}");
            return 2;
        }
    };
    let only: Option<Vec<String>> = std::env::var("GYROFLOW_SYNC_REGRESS_ONLY")
        .ok()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect());
    let gate_mode = std::env::var("GYROFLOW_SYNC_REGRESS_GATE").is_ok_and(|v| v.trim() == "1");
    if let Some(only) = &only {
        for name in only {
            if !corpus.clips.iter().any(|c| &c.name == name) {
                println!("[regress] GYROFLOW_SYNC_REGRESS_ONLY names an unknown clip: {name}");
                return 2;
            }
        }
    }
    let clips: Vec<&Clip> = corpus
        .clips
        .iter()
        .filter(|c| only.as_ref().is_none_or(|o| o.iter().any(|n| *n == c.name)))
        .collect();
    let out_dir = PathBuf::from(&corpus.out_dir);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        println!("[regress] cannot create {}: {e}", out_dir.display());
        return 2;
    }
    println!(
        "[regress] corpus {corpus_path}: {} of {} clips, gate mode {}, decode {}",
        clips.len(),
        corpus.clips.len(),
        if gate_mode { "on" } else { "off" },
        decode.as_str()
    );

    let started = Instant::now();
    let mut results = Vec::new();
    let mut seq = 0usize;
    for clip in clips {
        println!("[regress] ===== {} =====", clip.name);
        results.push(run_clip(clip, decode, &mut seq));
    }

    let rows: Vec<Row> = results.iter().flat_map(|r| r.rows.iter().cloned()).collect();
    let known: Vec<&str> = corpus
        .known_fixes
        .iter()
        .filter(|k| results.iter().any(|r| &r.clip.name == *k))
        .map(String::as_str)
        .collect();
    let report = evaluate_gates(&rows, &known);

    let csv = rows_csv(&rows);
    let summary = summary_md(corpus_path, gate_mode, decode, &results, &known, &report, started.elapsed().as_secs_f64());
    for (name, text) in [("rows.csv", &csv), ("summary.md", &summary)] {
        if let Err(e) = std::fs::write(out_dir.join(name), text) {
            println!("[regress] cannot write {name}: {e}");
        }
    }
    println!("{summary}");
    println!("[regress] wrote {} and {}", out_dir.join("rows.csv").display(), out_dir.join("summary.md").display());
    println!("[regress] gates: {}", if report.passed() { "PASS" } else { "FAIL" });
    log::logger().flush();

    if gate_mode && !report.passed() { 1 } else { 0 }
}

fn run_clip(clip: &Clip, decode: DecodeMode, seq: &mut usize) -> ClipResult {
    let mut result = ClipResult { clip: clip.clone(), info: None, runs: Vec::new(), rows: Vec::new(), error: None };
    let stab = match import_clip(clip) {
        Ok((stab, info)) => {
            println!(
                "[regress] {}: {}x{} fps={:.3} duration={:.0}ms fps_scale={:?} quats={} truth={:?} points={:?}",
                clip.name, info.width, info.height, info.fps, info.duration_ms, info.fps_scale, info.quats, clip.truth, clip.sync_points_ms
            );
            result.info = Some(info);
            stab
        }
        Err(e) => {
            println!("[regress] {}: FAILED: {e}", clip.name);
            result.error = Some(e);
            for method in METHODS {
                for window in 0..clip.sync_points_ms.len() {
                    result.rows.push(empty_row(clip, window, method, f64::NAN));
                }
            }
            return result;
        }
    };
    let info = result.info.clone().unwrap_or_default();
    for method in METHODS {
        *seq += 1;
        let run = run_method(&stab, clip, &info, method, decode, *seq);
        println!(
            "[regress] {} m{}: rows={} total={:.0}ms decode={:.0}ms feed={:.0}ms frames={} probe={} decoder={} errors={:?}",
            clip.name,
            method,
            run.result.as_ref().map_or(0, Vec::len),
            run.total_ms,
            run.timing.wall_ms - run.timing.feed_ms,
            run.timing.feed_ms,
            run.timing.frames,
            run.probe,
            run.decoders.join(" + "),
            run.errors
        );
        result.rows.extend(rows_for(clip, &info, &run));
        result.runs.push(run);
    }
    result
}

fn import_clip(clip: &Clip) -> Result<(Arc<StabilizationManager>, ClipInfo), String> {
    let stab = Arc::new(StabilizationManager::default());
    stab.lens_profile_db.write().load_all();
    let cancel = Arc::new(AtomicBool::new(false));
    let url = gyroflow_core::filesystem::path_to_url(&clip.project);
    stab.import_gyroflow_file(&url, true, |_| (), cancel, false).map_err(|e| format!("import: {e:?}"))?;
    if stab.gyro.read().quaternions.len() < 2 {
        stab.recompute_gyro();
    }
    // The project's offsets are not the truth; every method starts without any
    stab.gyro.write().clear_offsets();
    let info = {
        let p = stab.params.read();
        ClipInfo {
            width: p.size.0,
            height: p.size.1,
            fps: p.fps,
            duration_ms: p.duration_ms,
            fps_scale: p.fps_scale,
            quats: stab.gyro.read().quaternions.len(),
        }
    };
    if !(info.duration_ms > 0.0) || info.width == 0 || info.height == 0 {
        return Err(format!("video unknown after import: {}x{} duration {}ms", info.width, info.height, info.duration_ms));
    }
    Ok((stab, info))
}

fn run_method(stab: &Arc<StabilizationManager>, clip: &Clip, info: &ClipInfo, method: usize, decode: DecodeMode, seq: usize) -> MethodRun {
    let mut run = MethodRun {
        method,
        ranges_ms: Vec::new(),
        result: None,
        total_ms: f64::NAN,
        timing: DecodeTiming::default(),
        probe: false,
        decoders: Vec::new(),
        errors: Vec::new(),
        log: Vec::new(),
    };
    stab.pose_estimator.clear();
    let mut sync_params = match sync_params_for(clip, method) {
        Ok(p) => p,
        Err(e) => {
            run.errors.push(e);
            return run;
        }
    };
    sync_params.normalize_for_mode(MODE);
    let timestamps_fract: Vec<f64> = clip.sync_points_ms.iter().map(|ms| ms / info.duration_ms).collect();

    let tag = format!("[regress] run {seq}: clip={} method={method}", clip.name);
    log::info!(target: "sync_regress", "{tag}");
    // The controller resets the GPU codec blocklist at every autosync start
    if decode == DecodeMode::GpuFirst {
        super::gpu_codec_blocklist::clear();
    }

    let result: Arc<Mutex<Option<(Vec<(f64, f64, f64, f64)>, Instant)>>> = Arc::new(Mutex::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    let t0 = Instant::now();
    let mut sync = match AutosyncProcess::from_manager(stab, &timestamps_fract, sync_params, MODE.to_string(), cancel.clone()) {
        Ok(s) => s,
        Err(e) => {
            run.errors.push(format!("from_manager: {e:?}"));
            run.log = captured_log(&tag);
            return run;
        }
    };
    {
        let result = result.clone();
        sync.on_finished(move |arg| {
            if let AutosyncResult::Offsets(offsets) = arg {
                *result.lock() = Some((offsets, Instant::now()));
            }
        });
    }
    run.ranges_ms = sync.get_ranges();
    let sync = Rc::new(sync);
    let video_url = gyroflow_core::filesystem::path_to_url(&clip.video);
    let dng_curve = gyroflow_core::dng_tone_curve::DngToneCurve::from_url(&video_url).map(Rc::new);
    let timing = Rc::new(RefCell::new(DecodeTiming::default()));

    // GPU first: the controller's codec signature probe and blocklist check
    let (codec_sig, try_gpu) = match decode {
        DecodeMode::Software => (None, false),
        DecodeMode::GpuFirst => {
            let sig = match VideoProcessor::get_video_info(&video_url) {
                Ok(info) => Some(CodecSignature::from(&info)),
                Err(e) => {
                    log::debug!("[autosync] codec signature probe failed: {e:?} (proceeding without blocklist consultation)");
                    None
                }
            };
            let blocked = sig.as_ref().is_some_and(super::gpu_codec_blocklist::is_blocklisted);
            if blocked {
                log::info!("[autosync] skipping GPU for blocklisted signature {:?}", sig);
            }
            (sig, !blocked)
        }
    };
    let attempt = |use_gpu: bool, ranges: Vec<(f64, f64)>| decode_and_feed(&sync, &video_url, ranges, use_gpu, &cancel, &dng_curve, &timing);
    // One `try_run` round of the controller: GPU first when allowed, software on the same process when GPU decoding
    // fails. Only the first round blocklists the codec, as in the controller
    let round = |ranges: Vec<(f64, f64)>, first: bool| -> (Result<(), FFmpegError>, String) {
        if !try_gpu {
            let label = if decode == DecodeMode::Software { "sw" } else { "sw (blocklisted)" };
            return (attempt(false, ranges).0, label.to_string());
        }
        match attempt(true, ranges.clone()) {
            (Err(FFmpegError::GPUDecodingFailed), backend) => {
                if first {
                    match codec_sig {
                        Some(sig) => {
                            log::info!("[autosync] GPU decode failed for signature {:?}, retrying with software", sig);
                            super::gpu_codec_blocklist::record_failure(sig);
                        }
                        None => log::info!("[autosync] GPU decode failed (no signature available), retrying with software"),
                    }
                }
                (attempt(false, ranges).0, format!("sw-fallback (after {backend})"))
            }
            other => other,
        }
    };

    let (first, decoder) = round(run.ranges_ms.clone(), true);
    run.decoders.push(decoder);
    let round1_ok = first.is_ok();
    if let Err(e) = first {
        run.errors.push(format!("decode: {e}"));
    }
    sync.finished_feeding_frames();
    // Lazy probe escalation of the existing chain, as in the controller
    if round1_ok {
        if let Some(probe_ranges) = sync.pending_probe_ranges() {
            run.probe = true;
            let (probe, decoder) = round(probe_ranges, false);
            run.decoders.push(format!("probe {decoder}"));
            if let Err(e) = probe {
                run.errors.push(format!("decode: {e}"));
            }
            sync.finished_feeding_frames();
        }
    }
    let delivered = result.lock().take();
    let end = delivered.as_ref().map_or_else(Instant::now, |(_, t)| *t);
    run.total_ms = end.duration_since(t0).as_secs_f64() * 1000.0;
    run.result = delivered.map(|(rows, _)| rows);
    if run.result.is_none() {
        run.errors.push("no result callback".into());
    }
    run.timing = timing.borrow().clone();
    drop(sync);
    run.log = captured_log(&tag);
    run
}

/// One attempt of `Controller::start_autosync`'s `try_run`. Converter errors are recorded like the controller reports
/// them, without failing the pass; the result is the decoder's. Also returns which decoder delivered the frames:
/// "sw", "gpu:<backend>", or for a GPU request that got no hardware decoder "sw (no hw decoder)".
#[allow(clippy::too_many_arguments)]
fn decode_and_feed(
    sync: &Rc<AutosyncProcess>,
    video_url: &str,
    ranges: Vec<(f64, f64)>,
    use_gpu: bool,
    cancel: &Arc<AtomicBool>,
    dng_curve: &Option<Rc<gyroflow_core::dng_tone_curve::DngToneCurve>>,
    timing: &Rc<RefCell<DecodeTiming>>,
) -> (Result<(), FFmpegError>, String) {
    static PASS: AtomicUsize = AtomicUsize::new(0);
    let started = Instant::now();
    // `synchronize` mode: the process' own every_nth_frame
    let every_nth_frame = sync.sync_params.every_nth_frame.max(1);
    let mut frame_no = 0usize;
    let mut abs_frame_no = 0usize;
    let optical_sync = sync.sync_params.offset_method == OPTICAL;

    let mut decoder_options = ffmpeg_next::Dictionary::new();
    if let Some(scale) = super::sync_decoder_scale_string(PROC_HEIGHT, video_url) {
        decoder_options.set("scale", &scale);
    }
    let tag = format!("[regress] decode pass {}: gpu={use_gpu}", PASS.fetch_add(1, Relaxed));
    log::info!(target: "sync_regress", "{tag}");
    let mut proc = match VideoProcessor::from_file(video_url, use_gpu, 0, Some(decoder_options)) {
        Ok(p) => p,
        Err(e) => {
            timing.borrow_mut().wall_ms += started.elapsed().as_secs_f64() * 1000.0;
            return (Err(e), if use_gpu { "gpu (decoder open failed)".into() } else { "sw".into() });
        }
    };
    let backend = if use_gpu {
        match hw_backend_since(&tag) {
            Some((kind, name)) if !name.is_empty() => format!("gpu:{kind}"),
            _ => "sw (no hw decoder)".to_string(),
        }
    } else {
        "sw".to_string()
    };

    if optical_sync { proc.set_decode_frame_step(every_nth_frame, sync.source_fps()); }
    let convert_error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let (sync2, timing2, dng_curve2, convert_error2) = (sync.clone(), timing.clone(), dng_curve.clone(), convert_error.clone());
    proc.on_frame(move |timestamp_us, input_frame, _output_frame, converter, _rate_control| {
        if if optical_sync { sync2.wants_optical_frame(timestamp_us) } else { abs_frame_no % every_nth_frame == 0 } {
            let h = PROC_HEIGHT as u32;
            let ratio = input_frame.height() as f64 / h as f64;
            let sw = (input_frame.width() as f64 / ratio).round() as u32;
            let sh = (input_frame.height() as f64 / (input_frame.width() as f64 / sw as f64)).round() as u32;
            let nv12 = sync2.sync_params.of_method == 3 || sync2.sync_params.of_method == 4;
            let pix_fmt = if nv12 { Pixel::NV12 } else { Pixel::GRAY8 };
            if let Some(curve) = &dng_curve2 {
                super::apply_dng_tone_curve(input_frame, curve);
            }
            match converter.scale(input_frame, pix_fmt, sw, sh) {
                Ok(small_frame) => {
                    let (width, height, stride, pixels) = if nv12 {
                        let y_len = small_frame.stride(0) * small_frame.plane_height(0) as usize;
                        let uv_len = small_frame.stride(1) * small_frame.plane_height(1) as usize;
                        let mut buf = Vec::with_capacity(y_len + uv_len);
                        buf.extend_from_slice(&small_frame.data(0)[..y_len]);
                        buf.extend_from_slice(&small_frame.data(1)[..uv_len]);
                        (small_frame.plane_width(0), small_frame.plane_height(0), small_frame.stride(0), std::borrow::Cow::Owned(buf))
                    } else {
                        let pixels = if optical_sync { std::borrow::Cow::Borrowed(small_frame.data(0)) }
                            else { std::borrow::Cow::Owned(small_frame.data(0).to_vec()) };
                        (small_frame.plane_width(0), small_frame.plane_height(0), small_frame.stride(0), pixels)
                    };
                    let fed = Instant::now();
                    sync2.feed_frame(timestamp_us, frame_no, width, height, stride, &pixels);
                    let mut t = timing2.borrow_mut();
                    t.feed_ms += fed.elapsed().as_secs_f64() * 1000.0;
                    t.frames += 1;
                }
                Err(e) => {
                    convert_error2.borrow_mut().get_or_insert(e.to_string());
                }
            }
            frame_no += 1;
        }
        abs_frame_no += 1;
        Ok(())
    });
    let decoded = proc.start_decoder_only(ranges, cancel.clone());
    drop(proc);
    timing.borrow_mut().wall_ms += started.elapsed().as_secs_f64() * 1000.0;
    if let Some(e) = convert_error.borrow_mut().take() {
        println!("[regress] converter error (frames skipped): {e}");
    }
    (decoded, backend)
}

/// The hardware backend the decoder opened after `tag` was logged, from its `Selected HW backend` debug line:
/// (device type, device name), the name empty when no hardware decoder was found
fn hw_backend_since(tag: &str) -> Option<(String, String)> {
    log::logger().flush();
    let buf = crate::logger::ring_buffer_snapshot();
    let text = String::from_utf8_lossy(&buf);
    let pos = text.rfind(tag)?;
    text[pos..].lines().find_map(parse_hw_backend)
}

fn parse_hw_backend(line: &str) -> Option<(String, String)> {
    const KEY: &str = "Selected HW backend ";
    let rest = &line[line.find(KEY)? + KEY.len()..];
    let kind = rest.split_whitespace().next()?.trim_start_matches("AV_HWDEVICE_TYPE_").to_ascii_lowercase();
    let name = rest.split_once('(').and_then(|(_, r)| r.split_once(')')).map_or(String::new(), |(n, _)| n.to_string());
    Some((kind, name))
}

/// The `[optical]` and `[posterior]` log lines written since `tag`, from the logger's ring buffer
fn captured_log(tag: &str) -> Vec<String> {
    log::logger().flush();
    let buf = crate::logger::ring_buffer_snapshot();
    let text = String::from_utf8_lossy(&buf);
    let Some(pos) = text.rfind(tag) else {
        println!("[regress] tag not found in the log ring buffer, the [optical] lines of this run are only in the log: {tag}");
        return Vec::new();
    };
    text[pos..]
        .lines()
        .filter_map(|l| l.find("[optical]").or_else(|| l.find("[posterior]")).map(|i| l[i..].to_string()))
        .collect()
}

fn empty_row(clip: &Clip, window: usize, method: usize, total_ms: f64) -> Row {
    Row {
        clip: clip.name.clone(),
        window,
        method,
        gate: clip.gate,
        truth_ms: clip.truth,
        tol_ms: clip.tolerance,
        offset_ms: f64::NAN,
        conf: 0.0,
        cost: f64::NAN,
        total_ms,
    }
}

/// One row per sync point: each result row goes to the window whose centre (scaled ms, as the methods report it) is
/// nearest, within half a window. A window without a result row keeps a NaN offset and conf 0.
fn rows_for(clip: &Clip, info: &ClipInfo, run: &MethodRun) -> Vec<Row> {
    let n = clip.sync_points_ms.len();
    let per_window_ms = run.total_ms / n as f64;
    let mut rows: Vec<Row> = (0..n).map(|w| empty_row(clip, w, run.method, per_window_ms)).collect();
    let scale = info.fps_scale.unwrap_or(1.0);
    let centres: Vec<(f64, f64)> = run.ranges_ms.iter().map(|(a, b)| ((a + b) / 2.0 / scale, (b - a) / 2.0 / scale)).collect();
    let mut taken = vec![false; n];
    for &(t, offset, cost, conf) in run.result.iter().flatten() {
        let nearest = centres
            .iter()
            .enumerate()
            .take(n)
            .map(|(i, (c, half))| (i, (c - t).abs(), *half))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        match nearest {
            Some((i, d, half)) if d <= half + 1.0 && !taken[i] => {
                taken[i] = true;
                rows[i].offset_ms = offset;
                rows[i].cost = cost;
                rows[i].conf = conf;
            }
            _ => println!("[regress] {} m{}: result row at {t:.1}ms matches no window, ignored", clip.name, run.method),
        }
    }
    rows
}

// ---------------------------------------------------------------------------------------------------------------------
// Optical log lines

/// One `[optical] seg i: …` result or failure line
#[derive(Clone, Debug, Default, PartialEq)]
struct OpticalSeg {
    window: usize,
    failed: Option<String>,
    g: Option<f64>,
    second: Option<String>,
    near: Option<String>,
    coarse_ms: Option<f64>,
    fine_ms: Option<f64>,
}

/// The `[optical] run: …` line
#[derive(Clone, Debug, Default, PartialEq)]
struct OpticalRunLine {
    track_ms: f64,
    decode_wait_ms: f64,
    search_ms: f64,
}

fn number(v: &str) -> Option<f64> {
    v.trim_end_matches("ms").trim_end_matches("px").parse().ok()
}

fn parse_seg(line: &str) -> Option<OpticalSeg> {
    const KEY: &str = "[optical] seg ";
    let rest = &line[line.find(KEY)? + KEY.len()..];
    let (index, rest) = rest.split_once(':')?;
    // Only the result and failure lines, not "restarted" or "tracking failed"
    if !(rest.contains("offset=") || rest.contains("failed reason=")) {
        return None;
    }
    let mut seg = OpticalSeg { window: index.trim().parse().ok()?, ..Default::default() };
    for token in rest.split_whitespace() {
        let Some((k, v)) = token.split_once('=') else { continue };
        let text = || (v != "-").then(|| v.to_string());
        match k {
            "reason" => seg.failed = Some(v.to_string()),
            "G" => seg.g = number(v),
            "second" => seg.second = text(),
            "near" => seg.near = text(),
            "coarse" => seg.coarse_ms = number(v),
            "fine" => seg.fine_ms = number(v),
            _ => {}
        }
    }
    Some(seg)
}

fn parse_run_line(line: &str) -> Option<OpticalRunLine> {
    const KEY: &str = "[optical] run:";
    let rest = &line[line.find(KEY)? + KEY.len()..];
    let mut out = OpticalRunLine::default();
    for token in rest.split_whitespace() {
        let Some((k, v)) = token.split_once('=') else { continue };
        match k {
            "track_ms" => out.track_ms = number(v)?,
            "decode_wait_ms" => out.decode_wait_ms = number(v)?,
            "search_ms" => out.search_ms = number(v)?,
            _ => {}
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------------------------------
// Output

fn opt_num(v: Option<f64>, decimals: usize) -> String {
    match v {
        Some(x) if x.is_finite() => format!("{x:.decimals$}"),
        _ => String::new(),
    }
}

fn cell(v: f64, decimals: usize) -> String {
    if v.is_finite() { format!("{v:.decimals$}") } else { "-".into() }
}

fn rows_csv(rows: &[Row]) -> String {
    let mut s = String::from("clip,window,method,gate,truth_ms,tol_ms,offset_ms,conf,cost,total_ms,err_ms\n");
    for r in rows {
        let _ = writeln!(
            s,
            "{},{},{},{},{},{},{},{},{},{},{}",
            r.clip,
            r.window,
            r.method,
            r.gate,
            opt_num(r.truth_ms, 1),
            r.tol_ms,
            opt_num(Some(r.offset_ms), 3),
            opt_num(Some(r.conf), 4),
            opt_num(Some(r.cost), 5),
            opt_num(Some(r.total_ms), 1),
            opt_num(r.err_ms(), 3)
        );
    }
    s
}

fn verdict(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

fn summary_md(corpus_path: &str, gate_mode: bool, decode: DecodeMode, results: &[ClipResult], known: &[&str], report: &GateReport, elapsed_s: f64) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Sync regression summary\n");
    let _ = writeln!(s, "- Corpus: `{corpus_path}`, {} clips, gate mode {}, wall time {:.0} s", results.len(), if gate_mode { "on" } else { "off" }, elapsed_s);
    let decode_text = match decode {
        DecodeMode::Software => "decode `sw` (software only)",
        DecodeMode::GpuFirst => "decode `gpu` (GPU first, software fallback as in the controller)",
    };
    let _ = writeln!(s, "- Methods: m2 = existing chain (`offset_method` 2), m3 = optical motion (`offset_method` 3); both through `AutosyncProcess`, {decode_text}, {PROC_HEIGHT} px processing height, GRAY8, fresh `PoseEstimator` per method; m2 runs first on every clip");
    let _ = writeln!(s, "- Correct = conf >= {CONF_GATE} and |err| <= tolerance; err = offset - truth (ms); total = from before `from_manager` to the end of the result callback, per window");
    let _ = writeln!(s, "- G / near / second / coarse / fine: parsed in-process from the `[optical] seg` lines (logger ring buffer)\n");

    let _ = writeln!(s, "## Per window\n");
    let _ = writeln!(s, "| clip | w | gate | truth | m2 offset | m2 conf | m2 err | m2 ms | m3 offset | m3 conf | m3 err | m3 ms | G | near | second | coarse ms | fine ms | m3 |");
    let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let mut near_count = 0usize;
    let mut optical_windows = 0usize;
    for res in results {
        let segs: Vec<OpticalSeg> = res.runs.iter().filter(|r| r.method == OPTICAL).flat_map(|r| r.log.iter().filter_map(|l| parse_seg(l))).collect();
        for w in 0..res.clip.sync_points_ms.len() {
            let find = |m: usize| res.rows.iter().find(|r| r.window == w && r.method == m);
            let (b, o) = (find(BASELINE), find(OPTICAL));
            let seg = segs.iter().rev().find(|x| x.window == w);
            optical_windows += 1;
            if seg.is_some_and(|x| x.near.is_some()) {
                near_count += 1;
            }
            let status = |r: Option<&Row>| -> String {
                match r {
                    Some(r) if r.truth_ms.is_none() => String::new(),
                    Some(r) if is_correct(r) => "ok".into(),
                    Some(r) if r.conf >= CONF_GATE => "WRONG".into(),
                    Some(_) => "dropped".into(),
                    None => "-".into(),
                }
            };
            let m3_note = match (seg.and_then(|x| x.failed.clone()), res.error.as_ref()) {
                (_, Some(_)) => "clip error".to_string(),
                (Some(reason), _) => format!("failed {reason}"),
                (None, None) => status(o),
            };
            let num = |r: Option<&Row>, f: fn(&Row) -> f64, d: usize| r.map_or("-".into(), |r| cell(f(r), d));
            let err = |r: Option<&Row>| r.and_then(|r| r.err_ms()).map_or("-".into(), |e| format!("{e:+.1}"));
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} | {} {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                res.clip.name,
                w,
                if res.clip.gate { "y" } else { "n" },
                res.clip.truth.map_or("-".into(), |t| format!("{t:.0}")),
                num(b, |r| r.offset_ms, 2),
                num(b, |r| r.conf, 3),
                err(b),
                status(b),
                num(b, |r| r.total_ms, 0),
                num(o, |r| r.offset_ms, 2),
                num(o, |r| r.conf, 3),
                err(o),
                num(o, |r| r.total_ms, 0),
                seg.and_then(|x| x.g).map_or("-".into(), |g| format!("{g:.3}")),
                seg.and_then(|x| x.near.clone()).unwrap_or_else(|| "-".into()),
                seg.and_then(|x| x.second.clone()).unwrap_or_else(|| "-".into()),
                seg.and_then(|x| x.coarse_ms).map_or("-".into(), |v| format!("{v:.0}")),
                seg.and_then(|x| x.fine_ms).map_or("-".into(), |v| format!("{v:.0}")),
                m3_note
            );
        }
    }
    let _ = writeln!(s, "\nOptical windows with a near low point: {near_count} of {optical_windows}\n");

    let _ = writeln!(s, "## Per clip timings (ms)\n");
    let _ = writeln!(s, "decode = wall time of the decoder passes minus the time inside `feed_frame`; feed = time inside `feed_frame` (for m3 it includes waiting on the tracking queue); track / decode_wait / search from the `[optical] run` line (track summed over tracking threads)\n");
    let _ = writeln!(s, "| clip | windows | m2 total | m2 decode | m2 feed | m2 frames | m2 probe | m2 decoder | m3 total | m3 decode | m3 feed | m3 frames | m3 decoder | track | decode_wait | search | track > total/2 |");
    let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for res in results {
        let run = |m: usize| res.runs.iter().find(|r| r.method == m);
        let (b, o) = (run(BASELINE), run(OPTICAL));
        let line = o.and_then(|o| o.log.iter().rev().find_map(|l| parse_run_line(l)));
        let t = |r: Option<&MethodRun>, f: fn(&MethodRun) -> f64| r.map_or("-".into(), |r| cell(f(r), 0));
        let half = match (line.as_ref(), o) {
            (Some(l), Some(o)) if o.total_ms.is_finite() => if l.track_ms > o.total_ms / 2.0 { "yes" } else { "no" },
            _ => "-",
        };
        let decoders = |r: Option<&MethodRun>| r.map_or("-".into(), |r| r.decoders.join(" + "));
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            res.clip.name,
            res.clip.sync_points_ms.len(),
            t(b, |r| r.total_ms),
            t(b, |r| r.timing.wall_ms - r.timing.feed_ms),
            t(b, |r| r.timing.feed_ms),
            b.map_or("-".into(), |r| r.timing.frames.to_string()),
            b.map_or("-".into(), |r| if r.probe { "yes".to_string() } else { "no".to_string() }),
            decoders(b),
            t(o, |r| r.total_ms),
            t(o, |r| r.timing.wall_ms - r.timing.feed_ms),
            t(o, |r| r.timing.feed_ms),
            o.map_or("-".into(), |r| r.timing.frames.to_string()),
            decoders(o),
            line.as_ref().map_or("-".into(), |l| format!("{:.0}", l.track_ms)),
            line.as_ref().map_or("-".into(), |l| format!("{:.0}", l.decode_wait_ms)),
            line.as_ref().map_or("-".into(), |l| format!("{:.0}", l.search_ms)),
            half
        );
    }

    let _ = writeln!(s, "\n## Gates (gate = true clips with a truth)\n");
    let list = |v: &[String]| if v.is_empty() { "none".to_string() } else { v.join("; ") };
    let _ = writeln!(s, "1. No confident wrong optical result: {} ({}): {}", verdict(report.false_accepts.is_empty()), report.false_accepts.len(), list(&report.false_accepts));
    let _ = writeln!(s, "2. Success count, optical >= existing: {} ({} vs {})", verdict(report.optical_correct >= report.baseline_correct), report.optical_correct, report.baseline_correct);
    let _ = writeln!(s, "3. No regression: {} ({}): {}", verdict(report.regressions.is_empty()), report.regressions.len(), list(&report.regressions));
    let _ = writeln!(s, "4. Known fixes [{}]: {} ({}): {}", known.join(", "), verdict(report.known_fixes_missing.is_empty()), report.known_fixes_missing.len(), list(&report.known_fixes_missing));
    let _ = writeln!(s, "5. Speed, optical total <= existing total per window: {} ({}): {}", verdict(report.slower.is_empty()), report.slower.len(), list(&report.slower));
    let _ = writeln!(s, "\nOverall: **{}**", verdict(report.passed()));

    let errors: Vec<String> = results
        .iter()
        .flat_map(|r| {
            r.error.iter().map(move |e| format!("{}: {e}", r.clip.name)).chain(
                r.runs.iter().flat_map(move |m| m.errors.iter().map(move |e| format!("{} m{}: {e}", r.clip.name, m.method))),
            )
        })
        .collect();
    if !errors.is_empty() {
        let _ = writeln!(s, "\n## Errors\n");
        for e in errors {
            let _ = writeln!(s, "- {e}");
        }
    }

    let _ = writeln!(s, "\n## Log lines\n");
    for res in results {
        for run in &res.runs {
            if run.log.is_empty() {
                continue;
            }
            let _ = writeln!(s, "{} m{}:\n```", res.clip.name, run.method);
            for l in &run.log {
                let _ = writeln!(s, "{l}");
            }
            let _ = writeln!(s, "```");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(clip: &str, method: usize, offset: f64, conf: f64, ms: f64) -> Row {
        Row { clip: clip.into(), window: 0, method, gate: true, truth_ms: Some(-700.0), tol_ms: 3.0, offset_ms: offset, conf, cost: 1.0, total_ms: ms }
    }
    #[test]
    fn confident_wrong_optical_result_fails_gate_one() {
        let r = evaluate_gates(&[row("a", 2, 0.0, 0.0, 9000.0), row("a", 3, -650.0, 0.9, 4000.0)], &[]);
        assert_eq!(r.false_accepts.len(), 1);
        assert!(!r.passed());
    }
    #[test]
    fn dropped_optical_result_is_not_a_false_accept() {
        let r = evaluate_gates(&[row("a", 2, 0.0, 0.0, 9000.0), row("a", 3, -650.0, 0.2, 4000.0)], &[]);
        assert!(r.false_accepts.is_empty() && r.passed());
    }
    #[test]
    fn baseline_correct_optical_dropped_is_a_regression() {
        let r = evaluate_gates(&[row("a", 2, -701.0, 0.8, 9000.0), row("a", 3, -701.0, 0.2, 4000.0)], &[]);
        assert_eq!(r.regressions.len(), 1);
        assert!(!r.passed());
    }
    #[test]
    fn slower_optical_fails_speed_gate() {
        let r = evaluate_gates(&[row("a", 2, -701.0, 0.8, 4000.0), row("a", 3, -701.0, 0.9, 5000.0)], &[]);
        assert_eq!(r.slower.len(), 1);
    }
    #[test]
    fn known_fix_must_be_correct() {
        let r = evaluate_gates(&[row("a", 2, 0.0, 0.0, 9000.0), row("a", 3, -650.0, 0.2, 4000.0)], &["a"]);
        assert_eq!(r.known_fixes_missing, vec!["a".to_string()]);
    }
    #[test]
    fn ungated_and_truthless_rows_are_ignored() {
        let mut a = row("a", 3, -650.0, 0.9, 4000.0);
        a.gate = false;
        let mut b = row("b", 3, -650.0, 0.9, 4000.0);
        b.truth_ms = None;
        assert!(evaluate_gates(&[a, b], &[]).passed());
    }

    #[test]
    fn parses_optical_log_lines() {
        let ok = "[2026-10-02 12:00:00.000] [INFO ] [sync] [optical] seg 1: offset=-949.20 conf=0.812 G=1.624 second=-260.0ms cost=1.969px cands=3 near=+20.0 pairs=37 pts/pair=1200.5 coarse_pts/pair=200.0 coarse=812ms fine=304ms";
        let seg = parse_seg(ok).unwrap();
        assert_eq!(seg.window, 1);
        assert_eq!(seg.g, Some(1.624));
        assert_eq!(seg.near.as_deref(), Some("+20.0"));
        assert_eq!(seg.second.as_deref(), Some("-260.0ms"));
        assert_eq!((seg.coarse_ms, seg.fine_ms), (Some(812.0), Some(304.0)));
        assert_eq!(seg.failed, None);

        let failed = parse_seg("[optical] seg 0: failed reason=few_measurements").unwrap();
        assert_eq!(failed.failed.as_deref(), Some("few_measurements"));
        assert!(parse_seg("[optical] seg 0: restarted (frame index went back), dropped 12 pairs").is_none());
        assert_eq!(parse_seg("[optical] seg 0: offset=1 conf=0 G=0 second=- cost=0px cands=0 near=- pairs=0").unwrap().near, None);

        let run = parse_run_line("[optical] run: windows=1 track_ms=2210 decode_wait_ms=15 search_ms=1460").unwrap();
        assert_eq!(run, OpticalRunLine { track_ms: 2210.0, decode_wait_ms: 15.0, search_ms: 1460.0 });
    }

    #[test]
    fn parses_hw_backend_line() {
        let gpu = "[2026-10-02 12:00:00.000] [DEBUG] [gyroflow::rendering::ffmpeg_processor] Selected HW backend AV_HWDEVICE_TYPE_D3D11VA (NVIDIA GeForce RTX) with format Some(AV_PIX_FMT_D3D11)";
        assert_eq!(parse_hw_backend(gpu), Some(("d3d11va".to_string(), "NVIDIA GeForce RTX".to_string())));
        let none = "Selected HW backend AV_HWDEVICE_TYPE_NONE () with format None";
        assert_eq!(parse_hw_backend(none), Some(("none".to_string(), String::new())));
        assert_eq!(parse_hw_backend("Available decoders: []"), None);
    }

    #[test]
    fn clip_params_override_the_defaults() {
        let clip: Clip = serde_json::from_str(r#"{"name": "c", "project": "p", "video": "v", "sync_points_ms": [1000, 2000],
            "params": {"initial_offset": -812, "search_size": 60}}"#).unwrap();
        let p = sync_params_for(&clip, 3).unwrap();
        assert_eq!((p.initial_offset, p.search_size, p.time_per_syncpoint), (-812.0, 60.0, 1500.0));
        assert_eq!((p.offset_method, p.of_method, p.max_sync_points, p.every_nth_frame), (3, 2, 2, 1));
        assert_eq!(clip.tolerance, 3.0);
        assert!(clip.gate);

        let mut bad = clip.clone();
        bad.params = serde_json::from_str(r#"{"serach_size": 60}"#).unwrap();
        assert!(sync_params_for(&bad, 2).is_err());
        bad.params = serde_json::from_str(r#"{"offset_method": 2}"#).unwrap();
        assert!(sync_params_for(&bad, 3).is_err());
    }
}
