// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! `sync_replay` — offline decision-replay tool (change
//! `sync-likelihood-nuisance`, spec `sync-decision-replay`). Dev-only,
//! feature-gated (`--features sync-replay`), never enters release packaging.
//!
//! Input: one or more `sync_diag_output/<ts>/` session directories (or a
//! root containing them). For every rs-sync segment it rebuilds the
//! generative posterior decision and prints it next to the recorded fusion
//! decision:
//!
//! - sessions WITH `residuals.csv` (GYROFLOW_SYNC_DIAG=2) take the full
//!   robust-likelihood rebuild (σ-MAD + Tukey on standardized residuals +
//!   n_eff = the dumped per-window frame-pair count, design D2). Each
//!   sampled δ carries two row groups (`gain` column): g = 1.0 and the
//!   profiled ĝ solved live by rs-sync's `solve_gain` (design D1). Grid δ
//!   are thinned to ≤2000 points per group while δ* is full — the rebuild
//!   is point-count invariant per δ (see `full_posterior`). The sampled
//!   ~50ms grid is linearly resampled to the shared 5ms lattice before the
//!   conf integration so full and approx mode share one conf convention
//!   (2026-06-12 fix — integrating ±12.5ms mass directly on the sparse grid
//!   had underestimated conf by ~50×). The decision likelihood prefers the
//!   profiled group per δ (falling back to g = 1 where absent); the g = 1
//!   rebuild is reported alongside as the comparison column `post_g1`.
//!   First real level-2 session: `sync_diag_output/1781267482` (P1004620
//!   5447ms window, echo family) — corpus ledger in
//!   `openspec/changes/sync-likelihood-nuisance/corpus.md`.
//! - sessions with only cost curves replay in curve-approximation mode
//!   (`logL = -(n_eff/2)·ln(cost/cost_min)`, σ profiled out exactly) and are
//!   marked `approx` per row.
//!
//! `--gate` evaluates the six acceptance gates of spec sync-decision-replay
//! and exits non-zero when any gate fails.
//!
//! `--join` (task 2.6, design D3) adds the cross-window likelihood product:
//! every window collected in this invocation (across session dirs and
//! ranges; full rebuild preferred per window, approx windows participate
//! labeled) is resampled onto one shared aligned 5ms lattice (intersection
//! of spans), the prior-free logL curves are added, and the prior is applied
//! exactly once (first window's recorded init, usual `--prior` semantics).
//! Degrades gracefully on a single window or zero grid overlap.
//!
//! Usage:
//!   sync_replay [--root <dir>] [<session_dir>...] [--n-eff <f>] [--thr <f>]
//!               [--prior stored|anchor|none] [--gate] [--no-table] [--join]
//! Defaults: --root ./sync_diag_output, --n-eff 150, --thr 0.5 (placeholder
//! drop threshold until the D5 conf calibration of task 2.5 lands),
//! --prior stored. `--n-eff` is the absolute effective dof in approx mode
//! and a RELATIVE multiplier (n_eff/150, neutral at the default) on the
//! dumped per-window frame-pair count in full mode. `--prior none` is the
//! likelihood-only configuration of task 2.6; `anchor` forces the
//! batch/deep-match tier (Gaussian σ = 1500ms, design D4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gyroflow_core::synchronization::posterior::{
    approx_window_log_likelihood, combine_windows_on_common_grid, posterior_decide,
    resample_logl_to_uniform_grid, sigma_mad, window_log_likelihood, Prior,
};

const GRID_STEP_MS: f64 = 5.0;
const AGREE_MS: f64 = 25.0;
/// CLI `--n-eff` default. In approx mode this is the absolute effective dof;
/// in full mode the dumped per-window frame-pair count is the base and the
/// CLI value acts as the relative multiplier `n_eff / N_EFF_DEFAULT`.
const N_EFF_DEFAULT: f64 = 150.0;
/// Labeled ground truths of the current corpus (proximity-labeling identical
/// to the Python prototype; superseded by corpus.md once task 2.3 lands).
const TRUTHS: &[(&str, f64)] = &[
    ("C50_0ms", 0.0),
    ("C50SF_-949", -949.0),
    ("P4620_-1497", -1497.3),
];
const LABEL_TOL_MS: f64 = 60.0;
/// Echo-family (P1004620) sessions for gate 1. 1781267482 is the first real
/// GYROFLOW_SYNC_DIAG=2 session (5447ms window, full-mode rebuild).
const P4620_SESSIONS: &[&str] = &["1781245402", "1781245706", "1781267482"];
const P4620_TRUTH: f64 = -1497.3;

/// Prior selection for the replay (`--prior`). `Stored` is the historical
/// default (weakly-informative Gaussian centered on the recorded init);
/// `Anchor` forces the batch/deep-match anchor tier (σ = 1500ms, design D4);
/// `NoPrior` is likelihood-only — the task-2.6 "likelihood alone vs
/// likelihood + prior" comparison knob. Sessions without a recorded init
/// fall back to `Uniform` in every mode.
#[derive(Clone, Copy, PartialEq)]
enum PriorMode {
    Stored,
    Anchor,
    NoPrior,
}

impl PriorMode {
    fn label(&self) -> &'static str {
        match self {
            PriorMode::Stored => "stored",
            PriorMode::Anchor => "anchor",
            PriorMode::NoPrior => "none",
        }
    }
}

fn make_prior(mode: PriorMode, init: Option<f64>, span_ms: f64) -> Prior {
    match (mode, init) {
        (PriorMode::NoPrior, _) | (_, None) => Prior::Uniform,
        (PriorMode::Anchor, Some(init_ms)) => Prior::Anchor { init_ms },
        (PriorMode::Stored, Some(init_ms)) => Prior::Stored { init_ms, search_size_ms: span_ms / 2.0 },
    }
}

#[derive(Clone)]
struct FusionRow {
    fused_offset_ms: f64,
    rs_argmin_ms: f64,
    path_taken: String,
}

/// Residual groups of one sampled δ, split by the dumped `gain` column
/// (each δ carries a g = 1 group plus a profiled-gain group whose gain value
/// is rs-sync's live closed-form `solve_gain` output ĝ).
#[derive(Default)]
struct DeltaResiduals {
    /// frame_pair_ts → residuals at g ≡ 1 (comparison rebuild).
    g1: BTreeMap<i64, Vec<f64>>,
    /// frame_pair_ts → residuals at the profiled gain ĝ.
    gained: BTreeMap<i64, Vec<f64>>,
    /// The profiled ĝ itself (gain value of the `gained` rows).
    gain: Option<f64>,
}

/// One parsed diag session (loaded once, replayed at multiple n_eff).
struct SessionData {
    name: String,
    /// range_idx → (5ms bucket key in ms → min cost). Pass-2 curves
    /// (range_idx ≥ 1000) are excluded; duplicate curves of the same range
    /// collapse to the per-bucket minimum (same as the prototype).
    cost: BTreeMap<usize, BTreeMap<i64, f64>>,
    /// range_idx → initial offset (first occurrence in summary.txt).
    inits: BTreeMap<usize, f64>,
    /// range_idx → last fusion_decision.csv row.
    fusion: BTreeMap<usize, FusionRow>,
    /// range_idx → sampled offset (0.1µms-scaled key) → per-gain residual
    /// groups. Present only for GYROFLOW_SYNC_DIAG=2 sessions.
    residuals: BTreeMap<usize, BTreeMap<i64, DeltaResiduals>>,
}

/// Per-window prior-free logL curve retained for the `--join` cross-window
/// product (design D3, task 2.6). `grid`/`logl` are the window's NATIVE
/// sampling (sparse sampled δ in full mode, 5ms cost buckets in approx
/// mode) — the join resamples them onto one shared aligned 5ms lattice.
struct JointWindow {
    session: String,
    range: usize,
    /// "full" or "approx" (same labeling as the per-window table).
    mode: &'static str,
    grid: Vec<f64>,
    logl: Vec<f64>,
    /// Recorded initial offset — the FIRST collected window's init seeds the
    /// joint prior (applied exactly once at decision time).
    init: Option<f64>,
    /// The window's own posterior argmax (for the joint report).
    solo_post: f64,
}

#[derive(Clone)]
struct SegmentRow {
    session: String,
    range: usize,
    /// "full" (residual rebuild) or "approx" (curve approximation).
    mode: &'static str,
    /// Decision posterior. In full mode this is the gain-profiled rebuild
    /// (post_gained); in approx mode the curve approximation.
    post: f64,
    /// Full mode only: the g ≡ 1 rebuild of the same window (comparison
    /// column; None in approx mode).
    post_g1: Option<f64>,
    conf: f64,
    ci95: (f64, f64),
    orig: f64,
    rs: f64,
    init: Option<f64>,
    path: String,
    tag: Option<&'static str>,
    truth: Option<f64>,
}

fn main() {
    let mut root: Option<PathBuf> = None;
    let mut sessions_args: Vec<PathBuf> = Vec::new();
    let mut n_eff = N_EFF_DEFAULT;
    let mut thr = 0.5f64;
    let mut prior_mode = PriorMode::Stored;
    let mut gate = false;
    let mut no_table = false;
    let mut join = false;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root = Some(PathBuf::from(args.get(i).expect("--root needs a value")));
            }
            "--n-eff" => {
                i += 1;
                n_eff = args.get(i).and_then(|v| v.parse().ok()).expect("--n-eff needs a number");
            }
            "--thr" => {
                i += 1;
                thr = args.get(i).and_then(|v| v.parse().ok()).expect("--thr needs a number");
            }
            "--prior" => {
                i += 1;
                prior_mode = match args.get(i).map(|v| v.as_str()) {
                    Some("stored") => PriorMode::Stored,
                    Some("anchor") => PriorMode::Anchor,
                    Some("none") => PriorMode::NoPrior,
                    other => {
                        eprintln!("--prior needs stored|anchor|none, got {other:?}");
                        std::process::exit(2);
                    }
                };
            }
            "--gate" => gate = true,
            "--no-table" => no_table = true,
            "--join" => join = true,
            "--help" | "-h" => {
                println!("sync_replay [--root <dir>] [<session_dir>...] [--n-eff <f>] [--thr <f>] [--prior stored|anchor|none] [--gate] [--no-table] [--join]");
                return;
            }
            other => sessions_args.push(PathBuf::from(other)),
        }
        i += 1;
    }

    let mut session_dirs: Vec<PathBuf> = Vec::new();
    if sessions_args.is_empty() {
        let root = root.unwrap_or_else(|| PathBuf::from("sync_diag_output"));
        match std::fs::read_dir(&root) {
            Ok(rd) => {
                for e in rd.flatten() {
                    if e.path().is_dir() {
                        session_dirs.push(e.path());
                    }
                }
            }
            Err(e) => {
                eprintln!("cannot read root {}: {e}", root.display());
                std::process::exit(2);
            }
        }
    } else {
        session_dirs = sessions_args;
    }
    session_dirs.sort();

    let sessions: Vec<SessionData> = session_dirs.iter().filter_map(|d| load_session(d)).collect();
    if sessions.is_empty() {
        eprintln!("no usable diag sessions found");
        std::process::exit(2);
    }

    let (rows, joint_windows) = analyze_corpus(&sessions, n_eff, prior_mode);

    if !no_table {
        print_table(&rows);
    }
    print_summary(&rows, n_eff, thr, prior_mode);

    if join {
        print_joint(&joint_windows, prior_mode);
    }

    if gate {
        let mut all_pass = true;
        println!("\n=== acceptance gates (spec sync-decision-replay; thr={thr}, n_eff={n_eff}) ===");
        let g15 = run_gates_1_to_5(&rows, thr);
        for g in &g15 {
            println!("{}", g.render());
            all_pass &= g.pass;
        }
        // Gate 6: n_eff perturbation robustness (×0.5 and ×2 must keep
        // gates 1-5 passing).
        let mut gate6_pass = true;
        for scale in [0.5f64, 2.0] {
            let (rows_s, _) = analyze_corpus(&sessions, n_eff * scale, prior_mode);
            let gs = run_gates_1_to_5(&rows_s, thr);
            let fails: Vec<String> = gs.iter().filter(|g| !g.pass).map(|g| g.name.to_string()).collect();
            if fails.is_empty() {
                println!("GATE 6 [n_eff x{scale}]: gates 1-5 PASS");
            } else {
                println!("GATE 6 [n_eff x{scale}]: FAIL ({})", fails.join(", "));
                gate6_pass = false;
            }
        }
        println!("GATE 6 {}: n_eff x0.5 / x2 robustness", if gate6_pass { "PASS" } else { "FAIL" });
        all_pass &= gate6_pass;
        println!("\nOVERALL: {}", if all_pass { "PASS" } else { "FAIL" });
        std::process::exit(if all_pass { 0 } else { 1 });
    }
}

// ── session loading ───────────────────────────────────────────────────────

fn load_session(dir: &Path) -> Option<SessionData> {
    let cost = load_cost(&dir.join("cost_curves_rssync.csv"));
    if cost.is_empty() {
        return None;
    }
    Some(SessionData {
        name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        cost,
        inits: load_inits(&dir.join("summary.txt")),
        fusion: load_fusion(&dir.join("fusion_decision.csv")),
        residuals: load_residuals(&dir.join("residuals.csv")),
    })
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

/// Header-indexed column lookup for the simple comma-CSV the diag layer
/// writes (no quoting; only trailing free-text columns may contain commas,
/// and all consumed columns precede them).
fn col_indices(header: &str, wanted: &[&str]) -> Option<Vec<usize>> {
    let cols: Vec<&str> = header.split(',').collect();
    wanted.iter().map(|w| cols.iter().position(|c| c == w)).collect()
}

fn load_cost(path: &Path) -> BTreeMap<usize, BTreeMap<i64, f64>> {
    let mut out: BTreeMap<usize, BTreeMap<i64, f64>> = BTreeMap::new();
    let lines = read_lines(path);
    let Some(idx) = lines.first().and_then(|h| col_indices(h, &["range_idx", "offset_ms", "cost"])) else {
        return out;
    };
    for line in lines.iter().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        let (Some(ri), Some(ofs), Some(c)) = (
            f.get(idx[0]).and_then(|v| v.parse::<usize>().ok()),
            f.get(idx[1]).and_then(|v| v.parse::<f64>().ok()),
            f.get(idx[2]).and_then(|v| v.parse::<f64>().ok()),
        ) else {
            continue;
        };
        // Same filters as the prototype: skip pass-2 curves (≥1000), NaN and
        // non-positive costs; collapse duplicates to the per-bucket minimum.
        if ri >= 1000 || c.is_nan() || c <= 0.0 {
            continue;
        }
        let key = (ofs / GRID_STEP_MS).round() as i64 * GRID_STEP_MS as i64;
        let d = out.entry(ri).or_default();
        let e = d.entry(key).or_insert(f64::INFINITY);
        if c < *e {
            *e = c;
        }
    }
    out
}

fn load_inits(path: &Path) -> BTreeMap<usize, f64> {
    let mut out = BTreeMap::new();
    for line in read_lines(path) {
        if line.starts_with("Sharpness") {
            break;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && !parts[0].is_empty() && parts[0].chars().all(|c| c.is_ascii_digit()) {
            if let (Ok(ri), Ok(init)) = (parts[0].parse::<usize>(), parts[1].parse::<f64>()) {
                out.entry(ri).or_insert(init); // first occurrence wins
            }
        }
    }
    out
}

fn load_fusion(path: &Path) -> BTreeMap<usize, FusionRow> {
    let mut out = BTreeMap::new();
    let lines = read_lines(path);
    let Some(idx) = lines.first().and_then(|h| col_indices(h, &["range_idx", "fused_offset_ms", "rs_argmin_ms", "path_taken"])) else {
        return out;
    };
    for line in lines.iter().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        let Some(ri) = f.get(idx[0]).and_then(|v| v.parse::<usize>().ok()) else { continue };
        let fused = f.get(idx[1]).and_then(|v| v.parse::<f64>().ok()).unwrap_or(f64::NAN);
        let rs = f.get(idx[2]).and_then(|v| v.parse::<f64>().ok()).unwrap_or(f64::NAN);
        let path = f.get(idx[3]).map(|v| v.to_string()).unwrap_or_default();
        // Last row per range wins (a segment re-run overwrites, same as the
        // prototype's dict assignment).
        out.insert(ri, FusionRow { fused_offset_ms: fused, rs_argmin_ms: rs, path_taken: path });
    }
    out
}

/// residuals.csv (GYROFLOW_SYNC_DIAG=2). Offsets are keyed at 0.1µms
/// resolution ((ms × 10000).round()) — sampled δ are ≥ 50ms apart so this is
/// collision-free while keeping exact float grouping. Rows with gain = 1.0
/// go into the g1 group; any other gain goes into the profiled group (the
/// `gain` column round-trips exactly at the dumped precision). A missing
/// gain column (pre-gain dumps) degrades to an all-g1 session.
fn load_residuals(path: &Path) -> BTreeMap<usize, BTreeMap<i64, DeltaResiduals>> {
    let mut out: BTreeMap<usize, BTreeMap<i64, DeltaResiduals>> = BTreeMap::new();
    if !path.exists() {
        return out;
    }
    let lines = read_lines(path);
    let Some(idx) = lines.first().and_then(|h| col_indices(h, &["range_idx", "offset_ms", "frame_pair_ts", "residual"])) else {
        return out;
    };
    let gain_idx = lines.first().and_then(|h| h.split(',').position(|c| c == "gain"));
    for line in lines.iter().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        let (Some(ri), Some(ofs), Some(ts), Some(r)) = (
            f.get(idx[0]).and_then(|v| v.parse::<usize>().ok()),
            f.get(idx[1]).and_then(|v| v.parse::<f64>().ok()),
            f.get(idx[2]).and_then(|v| v.parse::<i64>().ok()),
            f.get(idx[3]).and_then(|v| v.parse::<f64>().ok()),
        ) else {
            continue;
        };
        let gain = gain_idx
            .and_then(|gi| f.get(gi).and_then(|v| v.parse::<f64>().ok()))
            .unwrap_or(1.0);
        let key = (ofs * 10000.0).round() as i64;
        let d = out.entry(ri).or_default().entry(key).or_default();
        if gain == 1.0 {
            d.g1.entry(ts).or_default().push(r);
        } else {
            d.gain = Some(gain);
            d.gained.entry(ts).or_default().push(r);
        }
    }
    out
}

// ── replay core ───────────────────────────────────────────────────────────

fn analyze_corpus(sessions: &[SessionData], n_eff: f64, prior_mode: PriorMode) -> (Vec<SegmentRow>, Vec<JointWindow>) {
    let mut rows = Vec::new();
    let mut windows = Vec::new();
    for s in sessions {
        for (ri, buckets) in &s.cost {
            let init = s.inits.get(ri).copied();
            // Full rebuild when this range has a residual dump; otherwise
            // curve approximation. The decision likelihood prefers the
            // profiled-gain group per δ (g = 1 fallback where a δ has none);
            // the pure g = 1 rebuild is carried along as post_g1. The
            // prior-free per-window curve is kept for the --join product.
            let decided = match s.residuals.get(ri) {
                Some(res) if !res.is_empty() => {
                    let gained_view: Vec<(i64, &BTreeMap<i64, Vec<f64>>)> = res
                        .iter()
                        .map(|(k, d)| (*k, if d.gained.is_empty() { &d.g1 } else { &d.gained }))
                        .collect();
                    let g1_view: Vec<(i64, &BTreeMap<i64, Vec<f64>>)> =
                        res.iter().map(|(k, d)| (*k, &d.g1)).collect();
                    full_logl_curve(&gained_view, n_eff).and_then(|(grid, logl)| {
                        let p = decide_full_curve(&grid, &logl, init, prior_mode)?;
                        let g1 = full_posterior(&g1_view, init, n_eff, prior_mode).map(|q| q.0);
                        Some(("full", p, g1, grid, logl))
                    })
                }
                _ => approx_logl_curve(buckets, n_eff).and_then(|(grid, logl)| {
                    let p = decide_on_curve(&grid, &logl, init, prior_mode)?;
                    Some(("approx", p, None, grid, logl))
                }),
            };
            let Some((mode, post, post_g1, grid, logl)) = decided else { continue };
            windows.push(JointWindow {
                session: s.name.clone(),
                range: *ri,
                mode,
                grid,
                logl,
                init,
                solo_post: post.0,
            });
            let f = s.fusion.get(ri);
            let orig = f.map(|f| f.fused_offset_ms).unwrap_or(f64::NAN);
            let rs = f.map(|f| f.rs_argmin_ms).unwrap_or(f64::NAN);
            let path = f.map(|f| f.path_taken.clone()).unwrap_or_default();
            let (tag, truth) = label(orig, rs, post.0);
            rows.push(SegmentRow {
                session: s.name.clone(),
                range: *ri,
                mode,
                post: post.0,
                post_g1,
                conf: post.1,
                ci95: post.2,
                orig,
                rs,
                init,
                path,
                tag,
                truth,
            });
        }
    }
    (rows, windows)
}

/// Curve-approximation per-window log-likelihood. Mirrors the validated
/// Python prototype exactly: logL = -(n_eff/2)·ln(cost/min) on the 5ms
/// bucket grid. Prior-free — the decision prior is added by
/// `decide_on_curve` (per-window path) or once for the whole `--join`
/// product.
fn approx_logl_curve(buckets: &BTreeMap<i64, f64>, n_eff: f64) -> Option<(Vec<f64>, Vec<f64>)> {
    if buckets.is_empty() {
        return None;
    }
    let cost_min = buckets.values().copied().fold(f64::INFINITY, f64::min);
    let grid: Vec<f64> = buckets.keys().map(|k| *k as f64).collect();
    let logl: Vec<f64> = buckets.values().map(|c| approx_window_log_likelihood(*c, cost_min, n_eff)).collect();
    Some((grid, logl))
}

/// Decide on an already-uniform (5ms-lattice) logL curve: prior + softmax
/// integrals. The default prior is a Gaussian centered on the recorded
/// initial offset with σ = grid_span/4 (= search_size/2, i.e. design D4's
/// weakly-informative tier — the init source is not recorded in historical
/// sessions, so the anchor tier cannot be distinguished offline; `--prior`
/// overrides).
fn decide_on_curve(grid: &[f64], logl: &[f64], init: Option<f64>, prior_mode: PriorMode) -> Option<(f64, f64, (f64, f64))> {
    let span = grid.last()? - grid.first()?;
    let prior = make_prior(prior_mode, init, span);
    let p = posterior_decide(grid, logl, &prior)?;
    Some((p.argmax_ms, p.conf_posterior, p.ci95))
}

/// Full-mode decision on a native sampled-δ curve: resample the sparse grid
/// onto the uniform 5ms lattice first (conf-integration convention, see
/// `full_logl_curve`) and then decide.
fn decide_full_curve(grid: &[f64], logl: &[f64], init: Option<f64>, prior_mode: PriorMode) -> Option<(f64, f64, (f64, f64))> {
    let (grid_u, logl_u) = resample_logl_to_uniform_grid(grid, logl, GRID_STEP_MS)?;
    decide_on_curve(&grid_u, &logl_u, init, prior_mode)
}

/// Full robust-likelihood rebuild from one per-δ residual view of a
/// residuals.csv dump. The caller picks the view: the profiled-gain groups
/// (decision path — this is what makes the misset-family rescue replayable
/// offline) or the g ≡ 1 groups (comparison path).
/// σ is taken at the sampled δ with the smallest median |r| (the grid-best
/// δ* of design D2), then the Tukey/n_eff likelihood is evaluated on the
/// sampled offset grid and linearly resampled to the shared 5ms lattice
/// before `posterior_decide` — the ±12.5ms conf integral is only calibrated
/// on that lattice (2026-06-12 fix; the dump samples every 10th cell ≈50ms
/// plus the off-lattice refined δ*, where the window covers a single point
/// that actually represents 50ms of probability mass).
///
/// n_eff: `window_log_likelihood` already scales by this δ's dumped
/// frame-pair count — the real per-window n_eff of design D2. The CLI
/// `--n-eff` only applies the relative multiplier `n_eff_cli/150` (neutral
/// at the default) so gate 6's ×0.5/×2 perturbation reaches both modes.
/// (An earlier draft rescaled every window to the absolute CLI value,
/// discarding the dumped pair count.)
///
/// Per-δ point counts MAY differ (the dump thins grid δ to ≤2000 points
/// while δ* stays full): `window_log_likelihood` is `-n_pairs × mean(ρ)` —
/// a per-point average, count-invariant by construction (unit-tested in
/// `posterior.rs::replay_rescale_invariant_to_per_delta_point_count`).
/// σ/median selection is order statistics, equally count-invariant.
fn full_posterior(res: &[(i64, &BTreeMap<i64, Vec<f64>>)], init: Option<f64>, n_eff_cli: f64, prior_mode: PriorMode) -> Option<(f64, f64, (f64, f64))> {
    let (grid, logl) = full_logl_curve(res, n_eff_cli)?;
    decide_full_curve(&grid, &logl, init, prior_mode)
}

/// Per-window full-mode logL curve on the NATIVE sampled-δ grid (~50ms
/// lattice + the off-lattice refined δ*), before any resampling. Shared by
/// the per-window decision (`full_posterior`/`decide_full_curve`) and the
/// `--join` product, which resamples straight onto the joint 5ms lattice.
fn full_logl_curve(res: &[(i64, &BTreeMap<i64, Vec<f64>>)], n_eff_cli: f64) -> Option<(Vec<f64>, Vec<f64>)> {
    // δ*: smallest median absolute residual.
    let mut best: Option<(f64, usize)> = None;
    for (i, (_key, groups)) in res.iter().enumerate() {
        let mut all: Vec<f64> = groups.values().flatten().map(|r| r.abs()).filter(|r| r.is_finite()).collect();
        if all.is_empty() {
            continue;
        }
        let k = all.len() / 2;
        all.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let med = all[k];
        if best.map(|(m, _)| med < m).unwrap_or(true) {
            best = Some((med, i));
        }
    }
    let (_, best_i) = best?;
    let flat_best: Vec<f64> = res[best_i].1.values().flatten().copied().collect();
    let sigma = sigma_mad(&flat_best);

    let mut grid = Vec::new();
    let mut logl = Vec::new();
    for (key, groups) in res {
        let g: Vec<Vec<f64>> = groups.values().cloned().collect();
        if g.is_empty() {
            continue;
        }
        grid.push(*key as f64 / 10000.0);
        logl.push(window_log_likelihood(&g, sigma) * (n_eff_cli / N_EFF_DEFAULT));
    }
    Some((grid, logl))
}

fn label(orig: f64, rs: f64, post: f64) -> (Option<&'static str>, Option<f64>) {
    for (name, t) in TRUTHS {
        for v in [orig, rs, post] {
            if v.is_finite() && (v - t).abs() <= LABEL_TOL_MS {
                return (Some(name), Some(*t));
            }
        }
    }
    (None, None)
}

// ── reporting ─────────────────────────────────────────────────────────────

fn print_table(rows: &[SegmentRow]) {
    // post_ms is the decision posterior (gain-profiled in full mode);
    // post_g1 is the g ≡ 1 comparison rebuild (full mode only, "-" in approx).
    println!(
        "{:<12} {:>3} {:<6} {:>10} {:>10} {:>6} {:>10} {:>10} {:>10} {:<12} path",
        "session", "rng", "mode", "post_ms", "post_g1", "conf", "fusion_ms", "rs_ms", "init_ms", "tag"
    );
    for r in rows {
        let g1 = r.post_g1.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into());
        println!(
            "{:<12} {:>3} {:<6} {:>10.1} {:>10} {:>6.3} {:>10.1} {:>10.1} {:>10.1} {:<12} {}",
            r.session,
            r.range,
            r.mode,
            r.post,
            g1,
            r.conf,
            r.orig,
            r.rs,
            r.init.unwrap_or(f64::NAN),
            r.tag.unwrap_or("-"),
            &r.path[..r.path.len().min(55)],
        );
    }
}

fn median(v: &mut Vec<f64>) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { 0.5 * (v[n / 2 - 1] + v[n / 2]) }
}

fn print_summary(rows: &[SegmentRow], n_eff: f64, thr: f64, prior_mode: PriorMode) {
    let have: Vec<&SegmentRow> = rows.iter().filter(|r| r.orig.is_finite()).collect();
    let agree = have.iter().filter(|r| (r.post - r.orig).abs() <= AGREE_MS).count();
    let dis: Vec<&&SegmentRow> = have.iter().filter(|r| (r.post - r.orig).abs() > AGREE_MS).collect();
    let kept_dis: Vec<&&&SegmentRow> = dis.iter().filter(|r| r.conf >= thr).collect();

    println!("\n=== replay summary (n_eff={n_eff}, thr={thr}, prior={}) ===", prior_mode.label());
    println!("rows={}, with fusion output={}", rows.len(), have.len());
    if !have.is_empty() {
        println!(
            "posterior vs current-fusion agreement (<= {AGREE_MS}ms): {}/{} = {:.0}%",
            agree,
            have.len(),
            100.0 * agree as f64 / have.len() as f64
        );
    }
    println!("disagreements: {}; of which posterior keeps with conf>={thr}: {}", dis.len(), kept_dis.len());
    for r in &kept_dis {
        println!(
            "  KEPT-DISAGREEMENT: {} r{}: post={:.1} (conf {:.2}) vs fusion={:.1} init={:.1} | {}",
            r.session, r.range, r.post, r.conf, r.orig,
            r.init.unwrap_or(f64::NAN),
            &r.path[..r.path.len().min(55)]
        );
    }

    // Per-family classification.
    for (name, t) in TRUTHS {
        let fam: Vec<&SegmentRow> = rows.iter().filter(|r| r.tag == Some(name)).collect();
        if fam.is_empty() {
            continue;
        }
        let mut drop = 0;
        let mut ok = 0;
        let mut wrong_small = 0;
        let mut wrong_cat = 0;
        let mut ok_errs = Vec::new();
        for r in &fam {
            let err = (r.post - t).abs();
            if r.conf < thr {
                drop += 1;
            } else if err <= 25.0 {
                ok += 1;
                ok_errs.push(err);
            } else if err <= 500.0 {
                wrong_small += 1;
            } else {
                wrong_cat += 1;
            }
        }
        println!(
            "  {:<12} n={:<3} DROP={} OK={} WRONG-small={} WRONG-CATASTROPHIC={} median_err(OK)={:.1}ms",
            name, fam.len(), drop, ok, wrong_small, wrong_cat, median(&mut ok_errs)
        );
    }

    // Echo-family detail (the verification anchor of this change).
    for r in rows.iter().filter(|r| P4620_SESSIONS.contains(&r.session.as_str())) {
        let g1 = r.post_g1.map(|v| format!(" post_g1={v:.1}")).unwrap_or_default();
        println!(
            "  P4620 detail: {} r{}: post={:.1}{} conf={:.3} ci95=[{:.0},{:.0}] (fusion={:.1}, truth={}, init={:.1}) [{}]",
            r.session, r.range, r.post, g1, r.conf, r.ci95.0, r.ci95.1, r.orig, P4620_TRUTH,
            r.init.unwrap_or(f64::NAN), r.mode
        );
    }
}

/// `--join` report: cross-window likelihood product over every window
/// collected in this invocation (design D3, task 2.6). Each window's
/// prior-free logL (native sampling) is resampled onto one shared aligned
/// 5ms lattice restricted to the intersection of the window spans and added
/// elementwise; the prior is applied exactly once at decision time, seeded
/// by the FIRST window's recorded init under the usual `--prior` semantics.
/// Graceful degradation: a single window decides as-is (with a notice);
/// zero grid overlap prints a notice and skips — never panics.
fn print_joint(windows: &[JointWindow], prior_mode: PriorMode) {
    println!("\n=== joint cross-window product (--join, prior={}) ===", prior_mode.label());
    if windows.is_empty() {
        println!("joint: no usable windows collected - nothing to combine");
        return;
    }
    for w in windows {
        println!(
            "  window {} r{} [{}]: solo_argmax={:.1}ms span=[{:.0},{:.0}]ms init={:.1}",
            w.session,
            w.range,
            w.mode,
            w.solo_post,
            w.grid.first().copied().unwrap_or(f64::NAN),
            w.grid.last().copied().unwrap_or(f64::NAN),
            w.init.unwrap_or(f64::NAN)
        );
    }
    if windows.len() == 1 {
        println!("joint: only 1 window collected - joint degenerates to that window's posterior");
    }
    let curves: Vec<(&[f64], &[f64])> = windows.iter().map(|w| (w.grid.as_slice(), w.logl.as_slice())).collect();
    let Some((grid, logl)) = combine_windows_on_common_grid(&curves, GRID_STEP_MS) else {
        println!("joint: SKIPPED - no common {GRID_STEP_MS}ms-grid overlap across the {} window(s)", windows.len());
        return;
    };
    // Prior counted once: the first window's recorded init.
    match decide_on_curve(&grid, &logl, windows[0].init, prior_mode) {
        Some((post, conf, ci)) => println!(
            "joint: argmax={:.1}ms conf={:.3} ci95=[{:.0},{:.0}] windows={} common_grid=[{:.0},{:.0}]@{}ms prior_init={:.1}",
            post,
            conf,
            ci.0,
            ci.1,
            windows.len(),
            grid.first().copied().unwrap_or(f64::NAN),
            grid.last().copied().unwrap_or(f64::NAN),
            GRID_STEP_MS,
            windows[0].init.unwrap_or(f64::NAN)
        ),
        None => println!("joint: SKIPPED - posterior undefined on the common grid"),
    }
}

// ── acceptance gates ──────────────────────────────────────────────────────

struct GateResult {
    name: &'static str,
    pass: bool,
    detail: String,
}

impl GateResult {
    fn render(&self) -> String {
        format!("{} {}: {}", self.name, if self.pass { "PASS" } else { "FAIL" }, self.detail)
    }
}

fn run_gates_1_to_5(rows: &[SegmentRow], thr: f64) -> Vec<GateResult> {
    let mut out = Vec::new();

    // Gate 1 — echo family: every P1004620 segment must land in truth ±25ms
    // or be droppable by confidence (no "high conf + wrong value").
    {
        let fam: Vec<&SegmentRow> = rows.iter().filter(|r| P4620_SESSIONS.contains(&r.session.as_str())).collect();
        let bad: Vec<String> = fam
            .iter()
            .filter(|r| (r.post - P4620_TRUTH).abs() > 25.0 && r.conf >= thr)
            .map(|r| format!("{} r{} post={:.1} conf={:.2}", r.session, r.range, r.post, r.conf))
            .collect();
        let pass = !fam.is_empty() && bad.is_empty();
        let detail = if fam.is_empty() {
            "no echo-family sessions in corpus".into()
        } else if bad.is_empty() {
            format!("{} segment(s), all in truth±25ms or droppable", fam.len())
        } else {
            format!("violations: {}", bad.join("; "))
        };
        out.push(GateResult { name: "GATE 1 (echo)", pass, detail });
    }

    // Gate 2 — misset family: zero kept catastrophics (>500ms), kept median
    // error ≤ 4.2ms. All-dropped counts as vacuous-pass on the median term
    // (drop is an allowed outcome per spec sync-offset-posterior).
    out.push(family_gate(rows, thr, "GATE 2 (misset)", "C50SF_-949", true));

    // Gate 3 — simple family: kept median error ≤ 4.2ms.
    out.push(family_gate(rows, thr, "GATE 3 (simple)", "C50_0ms", false));

    // Gate 4 — twin family (fusion rows that went through twin handling):
    // posterior must not be worse than fusion — no new kept catastrophics
    // and kept median error within +1ms of fusion's on the same rows.
    {
        let fam: Vec<&SegmentRow> = rows
            .iter()
            .filter(|r| r.truth.is_some() && r.path.contains("twin"))
            .collect();
        if fam.is_empty() {
            out.push(GateResult { name: "GATE 4 (twin)", pass: true, detail: "no labeled twin rows (vacuous)".into() });
        } else {
            let mut new_cat = 0;
            let mut post_errs = Vec::new();
            let mut orig_errs = Vec::new();
            for r in &fam {
                let t = r.truth.unwrap();
                let pe = (r.post - t).abs();
                let oe = (r.orig - t).abs();
                if r.conf >= thr {
                    post_errs.push(pe);
                    orig_errs.push(oe);
                    if pe > 500.0 && oe <= 500.0 {
                        new_cat += 1;
                    }
                }
            }
            let pm = median(&mut post_errs);
            let om = median(&mut orig_errs);
            let med_ok = pm.is_nan() || om.is_nan() || pm <= om + 1.0;
            let pass = new_cat == 0 && med_ok;
            out.push(GateResult {
                name: "GATE 4 (twin)",
                pass,
                detail: format!("n={}, new_catastrophic={}, median post={:.1}ms vs fusion={:.1}ms", fam.len(), new_cat, pm, om),
            });
        }
    }

    // Gate 5 — zero regression: on labeled rows where fusion was correct
    // (≤25ms), no kept posterior may drift >25ms away AND be worse.
    {
        let mut regressions = Vec::new();
        let mut n_base = 0;
        for r in rows.iter().filter(|r| r.truth.is_some() && r.orig.is_finite()) {
            let t = r.truth.unwrap();
            let oe = (r.orig - t).abs();
            if oe > 25.0 {
                continue;
            }
            n_base += 1;
            let pe = (r.post - t).abs();
            if r.conf >= thr && (r.post - r.orig).abs() > 25.0 && pe > oe {
                regressions.push(format!("{} r{} post={:.1} (err {:.1} vs {:.1})", r.session, r.range, r.post, pe, oe));
            }
        }
        let pass = regressions.is_empty();
        let detail = if pass {
            format!("{n_base} fusion-correct segments, 0 kept regressions")
        } else {
            format!("{} regression(s): {}", regressions.len(), regressions.join("; "))
        };
        out.push(GateResult { name: "GATE 5 (no-regression)", pass, detail });
    }

    out
}

/// Shared body of gates 2/3: kept median error ≤ 4.2ms; optionally also
/// require zero kept catastrophics.
fn family_gate(rows: &[SegmentRow], thr: f64, name: &'static str, tag: &str, check_catastrophic: bool) -> GateResult {
    let fam: Vec<&SegmentRow> = rows.iter().filter(|r| r.tag == Some(tag)).collect();
    if fam.is_empty() {
        return GateResult { name, pass: true, detail: format!("no {tag} rows (vacuous)") };
    }
    let t = TRUTHS.iter().find(|(n, _)| *n == tag).map(|(_, t)| *t).unwrap();
    let mut kept_errs: Vec<f64> = Vec::new();
    let mut cat = 0;
    for r in &fam {
        let err = (r.post - t).abs();
        if r.conf >= thr {
            kept_errs.push(err);
            if err > 500.0 {
                cat += 1;
            }
        }
    }
    let kept_n = kept_errs.len();
    let med = median(&mut kept_errs);
    // All-dropped → vacuous pass on the median term (no wrong value escaped).
    let med_ok = kept_n == 0 || med <= 4.2;
    let cat_ok = !check_catastrophic || cat == 0;
    GateResult {
        name,
        pass: med_ok && cat_ok,
        detail: format!("n={}, kept={}, catastrophic_kept={}, median_err(kept)={:.1}ms (limit 4.2)", fam.len(), kept_n, cat, med),
    }
}
