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
//! Arbitration (change `sync-decision-arbitration`, spec `sync-decision-replay`
//! ADDED requirements). The tool also rebuilds the ci95-tiered arbiter
//! (design D2/D3): narrow ci95 → posterior; else fusion-strong (Pearson
//! `r >= arb_fusion_r` OR rs-cost `sharp >= arb_fusion_sharp`) → fusion; else
//! wide → drop / mid → posterior. Defaults narrow=12 wide=30 r=0.55 sharp=1.2,
//! env-overridable via the same knobs the live arbiter reads
//! (`GYROFLOW_SYNC_ARB_CI95_NARROW_MS/_CI95_WIDE_MS/_FUSION_R/_FUSION_SHARP`;
//! short `_NARROW_MS`/`_WIDE_MS` accepted as aliases).
//!   - `--arb` prints the per-seg arbiter decision (branch/choice/final/err).
//!   - `--arb-sweep` scans the four thresholds, keeps only HARD-SAFE combos
//!     (the two fusion-false-peak clips MUST never emit fusion), and reports
//!     the robust region (#within-25ms-of-truth) + robust value sets.
//!   - `--arb-gate` is the CI gate at the (env-overridden) defaults: every
//!     offline-replayable corpus session within 25ms of truth, zero
//!     regressions vs the posterior-owns baseline, false-peak clips never take
//!     fusion. FAIL → non-zero exit.
//!
//! KNOWN GAP (design D6): the tool does NOT model probe-escalation or the
//! single-window ×0.85 conf, so escalation-dependent sessions (clips
//! 7-run2 / 8 / 9 / 16) rebuild ≠ live and are marked `live_only` in
//! `ARB_CORPUS`, EXCLUDED from the offline gate (printed as a note). The
//! fusion Pearson r the arbiter keys on is also NOT on disk; it is
//! reconstructed offline as the axis-weighted correlation peak over
//! `correlation_curves.csv` (see `FusionBasin`) — close to live but ~0.05-0.1
//! LOW, which lowers the offline-validated r-threshold ceiling to ~0.6
//! (vs ~0.65 live). The default 0.55 is safely inside both windows.
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
//!               [--arb] [--arb-sweep] [--arb-gate] [--truth <session>=<ms>]
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

// ── sync-decision-arbitration corpus + arbiter ─────────────────────────────

/// Arbitration corpus entry (change `sync-decision-arbitration`,
/// `corpus.md`): a diag session, its user-eyeballed truth offset, the clip
/// label, and whether it is `live-only` (offline rebuild ≠ live because the
/// live decision used probe-escalation and/or the single-window ×0.85 conf —
/// design D6, KNOWN GAP). `live-only` sessions are EXCLUDED from the offline
/// arbitration gate; their truth is still parsed for the table but not scored.
///
/// `mode_b` marks clip 16 (DIS optical-flow sub-frame bias): both posterior
/// and fusion are off by the same ~1 frame, so the arbiter — which only
/// chooses BETWEEN the two estimates — structurally cannot fix it (design
/// Non-Goal). It is excluded from scoring like `live-only`.
struct CorpusEntry {
    session: &'static str,
    clip: &'static str,
    truth: Option<f64>,
    /// `true` → excluded from the offline gate (escalation/×0.85 divergence).
    live_only: bool,
    /// Reason string for the printed exclusion note.
    note: &'static str,
}

/// The 16-clip (21-session) arbitration corpus, transcribed from
/// `openspec/changes/sync-decision-arbitration/corpus.md`. Truths are the
/// per-clip user-eyeballed offsets; `live_only` is set for the escalation /
/// Mode-B sessions the design names as offline ≠ live.
///
/// WHY clip 1 is NOT live_only despite a divergent offline single-window
/// posterior: the corpus categorizes it `fusion-win`, and the arbiter takes
/// FUSION in the wide+strong tier — so the (correct) arbiter output never
/// reads the (divergent) posterior offset. The offline gate scores the
/// arbiter output, which is correct. This is the exact property design D6
/// relies on to keep clip 1 offline-gateable.
const ARB_CORPUS: &[CorpusEntry] = &[
    CorpusEntry { session: "1781336304", clip: "1 R52", truth: Some(-2696.0), live_only: false, note: "fusion-win (arbiter takes fusion; posterior offset divergent but unused)" },
    CorpusEntry { session: "1781336623", clip: "2 C50", truth: Some(0.0), live_only: false, note: "agree" },
    CorpusEntry { session: "1781336873", clip: "3 C50", truth: Some(-1754.0), live_only: false, note: "agree" },
    CorpusEntry { session: "1781337202", clip: "4 C50", truth: Some(0.0), live_only: false, note: "agree" },
    CorpusEntry { session: "1781337371", clip: "5 P4620", truth: Some(-1142.0), live_only: false, note: "agree (echo resolved)" },
    CorpusEntry { session: "1781337561", clip: "6 P4666", truth: Some(-1836.0), live_only: false, note: "agree (offline single-window rebuild lands on truth)" },
    CorpusEntry { session: "1781337788", clip: "7-run1 A001", truth: Some(-677.0), live_only: true, note: "escalation (live-only per design D6)" },
    CorpusEntry { session: "1781337840", clip: "7-run2 A001", truth: Some(-677.0), live_only: true, note: "escalation/drop->fusion (live-only per design D6)" },
    CorpusEntry { session: "1781337990", clip: "8-run1 DSC", truth: Some(-693.0), live_only: true, note: "escalation (live-only per design D6)" },
    CorpusEntry { session: "1781338032", clip: "8-run2 DSC", truth: Some(-693.0), live_only: true, note: "escalation rescue via probe window (live-only per design D6)" },
    CorpusEntry { session: "1781338184", clip: "9-DIS NikonZR", truth: Some(-812.0), live_only: true, note: "DIS drop->fusion, escalation-coupled (live-only per design D6)" },
    CorpusEntry { session: "1781338217", clip: "9-NeuFlow NikonZR", truth: Some(-812.0), live_only: true, note: "NeuFlow path, escalation (live-only per design D6)" },
    CorpusEntry { session: "1781338528", clip: "10 MVI5502", truth: Some(-1205.0), live_only: false, note: "agree (mid ci95 but consistent)" },
    CorpusEntry { session: "1781338649", clip: "11 MVI6085", truth: Some(-1928.0), live_only: false, note: "agree" },
    CorpusEntry { session: "1781338749", clip: "12 C50", truth: Some(-2291.0), live_only: false, note: "posterior-win small (offline rebuild lands on truth)" },
    CorpusEntry { session: "1781338929", clip: "13 P1032767", truth: Some(-1664.0), live_only: false, note: "posterior-win (narrow tier protects against fusion false peak)" },
    CorpusEntry { session: "1781339080", clip: "14 P1032775", truth: Some(-1661.0), live_only: false, note: "agree" },
    CorpusEntry { session: "1781339184", clip: "15 P1032787", truth: Some(-1650.0), live_only: false, note: "posterior-win (narrow tier protects against fusion false peak)" },
    CorpusEntry { session: "1781339282", clip: "16-DIS P1032805", truth: Some(-1637.0), live_only: true, note: "Mode-B: DIS sub-frame bias, arbiter structurally cannot fix (Non-Goal)" },
    CorpusEntry { session: "1781339318", clip: "16-NeuFlow P1032805", truth: Some(-1637.0), live_only: true, note: "Mode-B counterpart (NeuFlow), escalation (live-only)" },
];

/// The two fusion-false-peak clips. The arbiter MUST NEVER emit `fusion` on
/// these (a HARD-SAFETY invariant in the sweep, design D3): their fusion peak
/// is the WRONG basin (-4750 / +2942) — taking it would bake a catastrophic
/// offset. They must resolve via the narrow posterior tier.
const FALSE_PEAK_SESSIONS: &[&str] = &["1781338929", "1781339184"];

/// Arbiter thresholds (design D2/D3). Defaults match the placeholder in
/// `_arb_sim.py`; each is overridable via `GYROFLOW_SYNC_ARB_*` for the live
/// code (mirrored here so the offline tool exercises the same knobs).
#[derive(Clone, Copy)]
struct ArbThresholds {
    /// ci95 width (ms) at/below which the posterior is trusted (narrow tier).
    narrow: f64,
    /// ci95 width (ms) at/above which a fusion-weak segment is dropped.
    wide: f64,
    /// Fusion Pearson r at/above which fusion counts as strong.
    fusion_r: f64,
    /// rs-cost 2nd/best ratio at/above which fusion counts as strong.
    fusion_sharp: f64,
}

impl ArbThresholds {
    /// Defaults (design D2/D3): narrow=12 wide=30 r=0.55 sharp=1.2.
    fn defaults() -> Self {
        ArbThresholds { narrow: 12.0, wide: 30.0, fusion_r: 0.55, fusion_sharp: 1.2 }
    }

    /// Read the SAME env knobs the live rs_sync.rs arbiter uses, over the
    /// defaults, so a developer can probe a candidate live threshold offline
    /// without rebuilding:
    ///   `GYROFLOW_SYNC_ARB_CI95_NARROW_MS`, `_CI95_WIDE_MS`, `_FUSION_R`,
    ///   `_FUSION_SHARP`.
    /// The shorter `_NARROW_MS` / `_WIDE_MS` spellings are accepted as aliases
    /// (the live names take precedence when both are set).
    fn from_env() -> Self {
        let mut t = ArbThresholds::defaults();
        let env_f = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<f64>().ok());
        // Aliases first, then the canonical live names override.
        if let Some(v) = env_f("GYROFLOW_SYNC_ARB_NARROW_MS") { t.narrow = v; }
        if let Some(v) = env_f("GYROFLOW_SYNC_ARB_CI95_NARROW_MS") { t.narrow = v; }
        if let Some(v) = env_f("GYROFLOW_SYNC_ARB_WIDE_MS") { t.wide = v; }
        if let Some(v) = env_f("GYROFLOW_SYNC_ARB_CI95_WIDE_MS") { t.wide = v; }
        if let Some(v) = env_f("GYROFLOW_SYNC_ARB_FUSION_R") { t.fusion_r = v; }
        if let Some(v) = env_f("GYROFLOW_SYNC_ARB_FUSION_SHARP") { t.fusion_sharp = v; }
        t
    }
}

/// Arbiter choice (design D3).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArbChoice {
    Posterior,
    Fusion,
    Drop,
}

impl ArbChoice {
    fn label(&self) -> &'static str {
        match self {
            ArbChoice::Posterior => "posterior",
            ArbChoice::Fusion => "fusion",
            ArbChoice::Drop => "drop",
        }
    }
}

/// One arbitrated segment decision.
struct ArbDecision {
    choice: ArbChoice,
    branch: &'static str,
    offset: f64,
    /// `true` when the chosen output passes the downstream conf>=0.4 filter.
    kept: bool,
}

/// Whether fusion's basin is strong (design D3 strong-base predicate):
/// Pearson `r >= fusion_r` OR rs-cost 2nd/best ratio `sharp >= fusion_sharp`.
fn fusion_strong(b: &FusionBasin, t: &ArbThresholds) -> bool {
    b.pearson_r >= t.fusion_r || b.rs_2nd_over_best >= t.fusion_sharp
}

/// Logic C, exactly as `_arb_sim.py::logic_c` / the live rs_sync.rs arbiter
/// (design D3):
///   1. ci95 <= narrow                 → posterior  (narrow: protect false peaks)
///   2. else fusion strong             → fusion     (spans mid & wide)
///   3. else ci95 >= wide              → drop        (both-weak)
///   4. else (narrow < ci95 < wide)    → posterior  (mid-keep)
/// `kept` = chosen conf >= the downstream 0.4 filter (posterior keeps its own
/// conf; fusion is assigned 0.5 to pass the filter; drop is 0.0).
///
/// `fusion_off` is the live fused offset (`fused_offset_ms` from the CSV) —
/// the actual value emitted when fusion wins, NOT the reconstructed peak
/// location in `b.pearson_peak_ms` (that is only used by the gate to confirm
/// a false-peak fusion lands on its false basin).
fn arbitrate(post: f64, ci95: (f64, f64), post_conf: f64, fusion_off: f64, b: &FusionBasin, t: &ArbThresholds) -> ArbDecision {
    let w = ci95_width(ci95);
    // Non-finite ci95 / fusion → graceful no-op, keep posterior (matches the
    // live rs_sync.rs arbiter, which on unusable data leaves the posterior
    // output untouched rather than risking a wide-branch fusion swap).
    if !w.is_finite() || !fusion_off.is_finite() || !post.is_finite() {
        return ArbDecision { choice: ArbChoice::Posterior, branch: "nofinite-keep", offset: post, kept: post_conf >= 0.4 };
    }

    if w <= t.narrow {
        return ArbDecision { choice: ArbChoice::Posterior, branch: "narrow", offset: post, kept: post_conf >= 0.4 };
    }
    if fusion_strong(b, t) {
        return ArbDecision { choice: ArbChoice::Fusion, branch: "fusion-win", offset: fusion_off, kept: true };
    }
    if w >= t.wide {
        return ArbDecision { choice: ArbChoice::Drop, branch: "both-weak", offset: post, kept: false };
    }
    ArbDecision { choice: ArbChoice::Posterior, branch: "mid-keep", offset: post, kept: post_conf >= 0.4 }
}

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
    /// rs-cost 2nd_best/best ratio (`rs_2nd_over_best`, e.g. 1.2 = sharp) — a
    /// fusion basin-strength signal consumed by the arbiter (design D3).
    rs_2nd_over_best: f64,
}

/// Per-segment fusion basin strength, surfaced for the offline arbiter
/// (change `sync-decision-arbitration`, design D7). The arbiter's
/// "fusion-strong" judgement (`r >= arb_fusion_r` OR `sharp >= arb_fusion_sharp`)
/// reads `pearson_r` (the fusion Pearson peak r) and `rs_2nd_over_best`.
///
/// IMPORTANT OFFLINE-RECONSTRUCTION NOTE: the live fusion Pearson r
/// (`max_pearson_r` / `pearson_peak_r`) is NOT dumped to any CSV — it only
/// appears in the live `[pearson-scan]` / `[ncc-fuse]` log lines. The closest
/// offline reconstruction is the AXIS-WEIGHTED correlation peak over the
/// dumped `correlation_curves.csv` grid, weighted by `axis_weights.csv`
/// (sync-parallax-suppression M1: `rw = Σ w_i·r_i / Σ w_i`). This tracks the
/// live value closely (clip 1 0.61 vs live 0.66, clip 13 0.57 vs live 0.65,
/// clip 9 0.74 vs live 0.80) but is SYSTEMATICALLY ~0.05-0.1 LOWER because the
/// live scan applies parabolic sub-grid interpolation at the peak and scans a
/// window centered on `initial_offset ± search_size` rather than the full
/// dumped grid. The UNWEIGHTED `corr_mean` column cannot be used — the signed
/// per-axis average cancels to ~0 on contaminated clips. The fusion *offset*
/// at which this peak occurs is reconstructed too (`pearson_peak_ms`) so the
/// gate can confirm a strong-but-wrong (false-peak) fusion lands on its false
/// peak, not the truth.
#[derive(Clone, Copy, Default)]
struct FusionBasin {
    /// Offline reconstruction of the fusion Pearson peak r (axis-weighted max
    /// over `correlation_curves.csv`). 0.0 if not reconstructable.
    pearson_r: f64,
    /// Offset (ms) of that weighted-correlation peak.
    pearson_peak_ms: f64,
    /// rs-cost 2nd/best ratio from `fusion_decision.csv` (1.0 = flat basin).
    rs_2nd_over_best: f64,
    /// `corr_peak_r` from summary.txt's Correlation-analysis block (max over
    /// the segment's runs) — the est-vs-raw-gyro per-axis-averaged peak r.
    /// Reported alongside as a secondary diagnostic (it is a DIFFERENT signal
    /// from the fusion Pearson scan and is unweighted, so it is NOT used as
    /// the arbiter's strength input; surfaced for cross-checking only).
    corr_peak_r: f64,
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
    /// range_idx → fusion basin strength (reconstructed offline, design D7).
    basin: BTreeMap<usize, FusionBasin>,
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
    /// Fusion basin strength reconstructed offline (design D7) — the arbiter's
    /// strength input. Defaulted (all-zero) for ranges without correlation
    /// dumps; the arbiter treats that as "fusion weak".
    basin: FusionBasin,
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
    let mut arb = false;
    let mut arb_sweep = false;
    let mut arb_gate = false;
    // `--truth <session>=<ms>` overrides / extends the embedded corpus truths.
    let mut truth_overrides: BTreeMap<String, f64> = BTreeMap::new();

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
            "--arb" => arb = true,
            "--arb-sweep" => arb_sweep = true,
            "--arb-gate" => arb_gate = true,
            "--truth" => {
                i += 1;
                let v = args.get(i).cloned().unwrap_or_default();
                if let Some((sess, ms)) = v.split_once('=') {
                    if let Ok(t) = ms.trim().parse::<f64>() {
                        truth_overrides.insert(sess.trim().to_string(), t);
                    } else {
                        eprintln!("--truth needs <session>=<ms>, got {v:?}");
                        std::process::exit(2);
                    }
                } else {
                    eprintln!("--truth needs <session>=<ms>, got {v:?}");
                    std::process::exit(2);
                }
            }
            "--help" | "-h" => {
                println!("sync_replay [--root <dir>] [<session_dir>...] [--n-eff <f>] [--thr <f>] [--prior stored|anchor|none] [--gate] [--no-table] [--join] [--arb] [--arb-sweep] [--arb-gate] [--truth <session>=<ms>]");
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

    // Arbitration layer (change sync-decision-arbitration). `--arb` prints the
    // per-seg arbiter decision; `--arb-sweep` scans thresholds; `--arb-gate`
    // is the CI gate at the default (or env-overridden) thresholds.
    if arb || arb_gate {
        print_arb_table(&rows, &truth_overrides, ArbThresholds::from_env());
    }
    if arb_sweep {
        run_arb_sweep(&rows, &truth_overrides);
    }
    if arb_gate {
        let pass = run_arb_gate(&rows, &truth_overrides);
        std::process::exit(if pass { 0 } else { 1 });
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
    let fusion = load_fusion(&dir.join("fusion_decision.csv"));
    let basin = load_basin(
        &dir.join("correlation_curves.csv"),
        &dir.join("axis_weights.csv"),
        &dir.join("summary.txt"),
        &fusion,
    );
    Some(SessionData {
        name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        cost,
        inits: load_inits(&dir.join("summary.txt")),
        fusion,
        residuals: load_residuals(&dir.join("residuals.csv")),
        basin,
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
    // rs_2nd_over_best is optional (older dumps may predate it) — degrade to
    // 1.0 (flat basin) when absent.
    let sharp_idx = lines.first().and_then(|h| h.split(',').position(|c| c == "rs_2nd_over_best"));
    for line in lines.iter().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        let Some(ri) = f.get(idx[0]).and_then(|v| v.parse::<usize>().ok()) else { continue };
        let fused = f.get(idx[1]).and_then(|v| v.parse::<f64>().ok()).unwrap_or(f64::NAN);
        let rs = f.get(idx[2]).and_then(|v| v.parse::<f64>().ok()).unwrap_or(f64::NAN);
        let path = f.get(idx[3]).map(|v| v.to_string()).unwrap_or_default();
        let rs_2nd = sharp_idx
            .and_then(|si| f.get(si).and_then(|v| v.parse::<f64>().ok()))
            .unwrap_or(1.0);
        // Last row per range wins (a segment re-run overwrites, same as the
        // prototype's dict assignment).
        out.insert(ri, FusionRow { fused_offset_ms: fused, rs_argmin_ms: rs, path_taken: path, rs_2nd_over_best: rs_2nd });
    }
    out
}

/// Reconstruct per-range fusion basin strength offline (design D7). The live
/// fusion Pearson r is not on disk, so we rebuild the axis-weighted
/// correlation peak from `correlation_curves.csv` (per-axis `corr_x/y/z`) and
/// `axis_weights.csv` (`w_x/y/z`) — see the `FusionBasin` doc comment for why
/// the weighted form is required and why it runs ~0.05-0.1 low. We also lift
/// `rs_2nd_over_best` from the parsed fusion rows and `corr_peak_r` from
/// summary.txt (secondary diagnostic only).
fn load_basin(
    corr_path: &Path,
    weights_path: &Path,
    summary_path: &Path,
    fusion: &BTreeMap<usize, FusionRow>,
) -> BTreeMap<usize, FusionBasin> {
    let mut out: BTreeMap<usize, FusionBasin> = BTreeMap::new();

    // Per-range axis weights (normalized; sync-parallax-suppression M1).
    let mut weights: BTreeMap<usize, [f64; 3]> = BTreeMap::new();
    let wlines = read_lines(weights_path);
    if let Some(widx) = wlines.first().and_then(|h| col_indices(h, &["range_idx", "w_x", "w_y", "w_z"])) {
        for line in wlines.iter().skip(1) {
            let f: Vec<&str> = line.split(',').collect();
            let (Some(ri), Some(wx), Some(wy), Some(wz)) = (
                f.get(widx[0]).and_then(|v| v.parse::<usize>().ok()),
                f.get(widx[1]).and_then(|v| v.parse::<f64>().ok()),
                f.get(widx[2]).and_then(|v| v.parse::<f64>().ok()),
                f.get(widx[3]).and_then(|v| v.parse::<f64>().ok()),
            ) else {
                continue;
            };
            weights.entry(ri).or_insert([wx, wy, wz]);
        }
    }

    // Axis-weighted correlation peak per range over correlation_curves.csv.
    let clines = read_lines(corr_path);
    if let Some(cidx) = clines.first().and_then(|h| col_indices(h, &["range_idx", "offset_ms", "corr_x", "corr_y", "corr_z"])) {
        // range_idx → (best_rw, peak_ms)
        let mut peak: BTreeMap<usize, (f64, f64)> = BTreeMap::new();
        for line in clines.iter().skip(1) {
            let f: Vec<&str> = line.split(',').collect();
            let (Some(ri), Some(ofs), Some(cx), Some(cy), Some(cz)) = (
                f.get(cidx[0]).and_then(|v| v.parse::<usize>().ok()),
                f.get(cidx[1]).and_then(|v| v.parse::<f64>().ok()),
                f.get(cidx[2]).and_then(|v| v.parse::<f64>().ok()),
                f.get(cidx[3]).and_then(|v| v.parse::<f64>().ok()),
                f.get(cidx[4]).and_then(|v| v.parse::<f64>().ok()),
            ) else {
                continue;
            };
            // Weighted mean if weights present, else plain mean (matches live
            // `pearson_at`: weighted when M1 active, unweighted fallback).
            let rw = match weights.get(&ri) {
                Some([wx, wy, wz]) => {
                    let wsum = wx + wy + wz;
                    if wsum > 1e-12 {
                        (wx * cx + wy * cy + wz * cz) / wsum
                    } else {
                        (cx + cy + cz) / 3.0
                    }
                }
                None => (cx + cy + cz) / 3.0,
            };
            if !rw.is_finite() {
                continue;
            }
            let e = peak.entry(ri).or_insert((f64::NEG_INFINITY, f64::NAN));
            if rw > e.0 {
                *e = (rw, ofs);
            }
        }
        for (ri, (rw, ms)) in peak {
            out.entry(ri).or_default().pearson_r = rw.max(0.0);
            out.entry(ri).or_default().pearson_peak_ms = ms;
        }
    }

    // corr_peak_r from summary.txt Correlation block (max over the range's
    // runs). Block is delimited by the "Correlation analysis" header and ends
    // at a blank line / the parenthetical note.
    let slines = read_lines(summary_path);
    let mut in_corr = false;
    for line in &slines {
        if line.starts_with("Correlation analysis") {
            in_corr = true;
            continue;
        }
        if in_corr {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Columns: range_idx initial_ms corr@init cost_final_ms corr@final
            //          corr_peak_ms corr_peak_r  (7 numeric fields)
            if parts.len() >= 7 && parts[0].chars().all(|c| c.is_ascii_digit()) {
                if let (Ok(ri), Ok(cpr)) = (parts[0].parse::<usize>(), parts[6].parse::<f64>()) {
                    let e = out.entry(ri).or_default();
                    if cpr.abs() > e.corr_peak_r.abs() {
                        e.corr_peak_r = cpr;
                    }
                }
            } else if line.trim().is_empty() || line.starts_with('(') {
                break; // end of block
            }
        }
    }

    // rs_2nd_over_best from the parsed fusion rows.
    for (ri, fr) in fusion {
        out.entry(*ri).or_default().rs_2nd_over_best = fr.rs_2nd_over_best;
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
            let basin = s.basin.get(ri).copied().unwrap_or_default();
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
                basin,
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

/// ci95 interval width (ms). `f64::NAN` if the interval is degenerate.
fn ci95_width(ci: (f64, f64)) -> f64 {
    if ci.0.is_finite() && ci.1.is_finite() {
        ci.1 - ci.0
    } else {
        f64::NAN
    }
}

fn print_table(rows: &[SegmentRow]) {
    // post_ms is the decision posterior (gain-profiled in full mode);
    // post_g1 is the g ≡ 1 comparison rebuild (full mode only, "-" in approx).
    // ci95w is the posterior ci95 width (the arbiter's tier signal, design
    // D1); fus_r/sharp are the offline-reconstructed fusion basin strength
    // (design D7 — fus_r is the axis-weighted correlation peak, NOT exactly
    // the live Pearson r; see FusionBasin doc).
    println!(
        "{:<12} {:>3} {:<6} {:>10} {:>10} {:>6} {:>7} {:>10} {:>9} {:>9} {:>10} {:<10} path",
        "session", "rng", "mode", "post_ms", "post_g1", "conf", "ci95w", "fusion_ms", "rs_ms", "fus_r/sh", "init_ms", "tag"
    );
    for r in rows {
        let g1 = r.post_g1.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into());
        let w = ci95_width(r.ci95);
        let wstr = if w.is_finite() { format!("{w:.0}") } else { "-".into() };
        println!(
            "{:<12} {:>3} {:<6} {:>10.1} {:>10} {:>6.3} {:>7} {:>10.1} {:>9.1} {:>4.2}/{:<4.2} {:>10.1} {:<10} {}",
            r.session,
            r.range,
            r.mode,
            r.post,
            g1,
            r.conf,
            wstr,
            r.orig,
            r.rs,
            r.basin.pearson_r,
            r.basin.rs_2nd_over_best,
            r.init.unwrap_or(f64::NAN),
            r.tag.unwrap_or("-"),
            &r.path[..r.path.len().min(40)],
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

// ── arbitration (change sync-decision-arbitration) ─────────────────────────

/// Look up a session's corpus entry (by exact session-dir name).
fn corpus_entry(session: &str) -> Option<&'static CorpusEntry> {
    ARB_CORPUS.iter().find(|e| e.session == session)
}

/// Resolved truth for a session: `--truth` override first, then the embedded
/// corpus. `None` → not scoreable (skip).
fn truth_for(session: &str, overrides: &BTreeMap<String, f64>) -> Option<f64> {
    if let Some(t) = overrides.get(session) {
        return Some(*t);
    }
    corpus_entry(session).and_then(|e| e.truth)
}

/// Whether a session is excluded from offline arbitration scoring
/// (escalation / single-window ×0.85 / Mode-B divergence; design D6).
fn is_live_only(session: &str) -> bool {
    corpus_entry(session).map(|e| e.live_only).unwrap_or(false)
}

/// The arbitrated tolerance for "within truth" (spec gate-1 tolerance).
const ARB_GATE_MS: f64 = 25.0;

/// Per-seg arbiter table (`--arb`): one line per segment row with the chosen
/// branch, choice, final offset, kept flag, and (when a truth is known) the
/// signed error and OK/NO verdict.
fn print_arb_table(rows: &[SegmentRow], overrides: &BTreeMap<String, f64>, t: ArbThresholds) {
    println!(
        "\n=== arbiter decisions (--arb; narrow<={} wide>={} r>={} sharp>={}) ===",
        t.narrow, t.wide, t.fusion_r, t.fusion_sharp
    );
    println!(
        "{:<12} {:>3} {:>7} {:>10} {:>4.2}/{:<4.2} {:>6} | {:<11} {:<10} {:>10} {:>6} {:>8} {:>3}",
        "session", "rng", "ci95w", "post", t.fusion_r, t.fusion_sharp, "fus", "branch", "choice", "final", "kept", "err", "ok"
    );
    for r in rows {
        let dec = arbitrate(r.post, r.ci95, r.conf, r.orig, &r.basin, &t);
        let truth = truth_for(&r.session, overrides);
        let (errs, ok) = match (truth, dec.kept) {
            (Some(tr), true) => {
                let e = dec.offset - tr;
                (format!("{e:+.1}"), if e.abs() <= ARB_GATE_MS { "OK" } else { "NO" })
            }
            (_, false) => ("DROP".to_string(), "-"),
            (None, _) => ("?".to_string(), "-"),
        };
        let w = ci95_width(r.ci95);
        let wstr = if w.is_finite() { format!("{w:.0}") } else { "inf".into() };
        println!(
            "{:<12} {:>3} {:>7} {:>10.1} {:>4.2}/{:<4.2} {:>6.1} | {:<11} {:<10} {:>10.1} {:>6} {:>8} {:>3}",
            r.session, r.range, wstr, r.post, r.basin.pearson_r, r.basin.rs_2nd_over_best, r.orig,
            dec.branch, dec.choice.label(), dec.offset, dec.kept, errs, ok
        );
    }
}

/// Per-session arbitration outcome under given thresholds. A session's result
/// is its BEST kept arbitrated row (closest to truth) — mirroring live, which
/// takes the best window. `fusion_on_false_peak` is set if ANY row of a
/// false-peak session emitted `fusion` (the HARD-SAFETY violation, design D3).
struct SessionArbResult {
    /// Closest-to-truth error over kept rows (`None` if all rows dropped).
    best_err: Option<f64>,
    /// All kept rows were dropped.
    all_dropped: bool,
    /// HARD-SAFETY: a false-peak session emitted fusion on some row.
    fusion_on_false_peak: bool,
}

/// Arbitrate every row of one session and reduce to a per-session result
/// against `truth`. `false_peak` toggles the HARD-SAFETY check.
fn arbitrate_session(rows: &[&SegmentRow], truth: Option<f64>, false_peak: bool, t: ArbThresholds) -> SessionArbResult {
    let mut best_err: Option<f64> = None;
    let mut any_kept = false;
    let mut fusion_on_false_peak = false;
    for r in rows {
        let dec = arbitrate(r.post, r.ci95, r.conf, r.orig, &r.basin, &t);
        if false_peak && dec.choice == ArbChoice::Fusion {
            fusion_on_false_peak = true;
        }
        if dec.kept {
            any_kept = true;
            if let Some(tr) = truth {
                let e = (dec.offset - tr).abs();
                best_err = Some(best_err.map_or(e, |b| b.min(e)));
            }
        }
    }
    SessionArbResult { best_err, all_dropped: !any_kept, fusion_on_false_peak }
}

/// Group loaded rows by session, preserving corpus order.
fn rows_by_session(rows: &[SegmentRow]) -> Vec<(String, Vec<&SegmentRow>)> {
    let mut order: Vec<String> = Vec::new();
    let mut map: BTreeMap<String, Vec<&SegmentRow>> = BTreeMap::new();
    for r in rows {
        if !map.contains_key(&r.session) {
            order.push(r.session.clone());
        }
        map.entry(r.session.clone()).or_default().push(r);
    }
    order.into_iter().map(|s| { let v = map.remove(&s).unwrap(); (s, v) }).collect()
}

/// Score one threshold combo over the loaded sessions. Returns
/// `(within_25ms_count, scoreable_count, hard_safe)`. Live-only / Mode-B
/// sessions and sessions with no resolvable truth are skipped from scoring;
/// the HARD-SAFETY check (false-peak clips never emit fusion) is evaluated on
/// ALL loaded sessions regardless of scoring.
fn score_combo(grouped: &[(String, Vec<&SegmentRow>)], overrides: &BTreeMap<String, f64>, t: ArbThresholds) -> (usize, usize, bool) {
    let mut within = 0usize;
    let mut scoreable = 0usize;
    let mut hard_safe = true;
    for (session, rows) in grouped {
        let false_peak = FALSE_PEAK_SESSIONS.contains(&session.as_str());
        let truth = truth_for(session, overrides);
        let res = arbitrate_session(rows, truth, false_peak, t);
        if res.fusion_on_false_peak {
            hard_safe = false;
        }
        // Skip from scoring: live-only/Mode-B, or unknown truth.
        if is_live_only(session) || truth.is_none() {
            continue;
        }
        scoreable += 1;
        // Correct = best kept row within tolerance. (No corpus entry expects a
        // drop with a known truth — drop on a truthed session is a miss.)
        let ok = res.best_err.map(|e| e <= ARB_GATE_MS).unwrap_or(false);
        let _ = res.all_dropped;
        if ok {
            within += 1;
        }
    }
    (within, scoreable, hard_safe)
}

/// `--arb-sweep` (task 4): sweep the four thresholds over the loaded corpus,
/// keep only HARD-SAFE combos, and report the robust region (the set of safe
/// combos hitting the max within-25ms count) plus per-dimension robust value
/// sets. Mirrors `_arb_sim.py`'s sweep grid.
fn run_arb_sweep(rows: &[SegmentRow], overrides: &BTreeMap<String, f64>) {
    let grouped = rows_by_session(rows);
    let narrows = [6.0, 8.0, 10.0, 12.0, 15.0, 18.0];
    let wides = [25.0, 30.0, 40.0];
    let rs = [0.45, 0.5, 0.55, 0.6, 0.65];
    let sharps = [1.15, 1.2, 1.3];

    // (within, narrow, wide, r, sharp) for every HARD-SAFE combo.
    let mut safe: Vec<(usize, f64, f64, f64, f64)> = Vec::new();
    let mut scoreable_n = 0usize;
    let mut total_combos = 0usize;
    let mut unsafe_combos = 0usize;
    for &nr in &narrows {
        for &wd in &wides {
            for &rt in &rs {
                for &st in &sharps {
                    total_combos += 1;
                    let t = ArbThresholds { narrow: nr, wide: wd, fusion_r: rt, fusion_sharp: st };
                    let (within, sc, hard_safe) = score_combo(&grouped, overrides, t);
                    scoreable_n = sc;
                    if hard_safe {
                        safe.push((within, nr, wd, rt, st));
                    } else {
                        unsafe_combos += 1;
                    }
                }
            }
        }
    }

    println!("\n=== ARBITRATION THRESHOLD SWEEP (--arb-sweep) ===");
    println!(
        "grid: narrow{:?} wide{:?} fusion_r{:?} sharp{:?} (total {} combos)",
        narrows, wides, rs, sharps, total_combos
    );
    println!("scoreable sessions (offline-replayable, truthed): {scoreable_n}");
    println!("HARD-SAFETY: false-peak clips {:?} must never emit fusion", FALSE_PEAK_SESSIONS);
    println!("  safe combos: {} / {} ({} rejected for false-peak fusion)", safe.len(), total_combos, unsafe_combos);
    if safe.is_empty() {
        println!("  NO safe combos — the false-peak guard fails everywhere (investigate fus_r reconstruction)");
        return;
    }
    let maxok = safe.iter().map(|c| c.0).max().unwrap();
    println!("  max within-{ARB_GATE_MS}ms (safe combos only): {maxok}/{scoreable_n}");
    let robust: Vec<&(usize, f64, f64, f64, f64)> = safe.iter().filter(|c| c.0 == maxok).collect();
    println!("  robust region: {} safe combo(s) hitting max", robust.len());
    for (name, getter) in [
        ("narrow", 1usize),
        ("wide", 2usize),
        ("fusion_r", 3usize),
        ("sharp", 4usize),
    ] {
        let mut vals: Vec<f64> = robust
            .iter()
            .map(|c| match getter {
                1 => c.1,
                2 => c.2,
                3 => c.3,
                _ => c.4,
            })
            .collect();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        vals.dedup();
        let mid = vals[vals.len() / 2];
        println!("    {name:9}: robust values {vals:?}  -> center {mid}");
    }
    // Sanity at the default thresholds + the candidate center.
    for (label, t) in [
        ("default 12/30/0.55/1.2", ArbThresholds::defaults()),
        ("env     ", ArbThresholds::from_env()),
    ] {
        let (within, sc, hard_safe) = score_combo(&grouped, overrides, t);
        println!(
            "  {label}: within={within}/{sc} hard_safe={hard_safe} (n={}/{}/{}/{})",
            t.narrow, t.wide, t.fusion_r, t.fusion_sharp
        );
    }
}

/// `--arb-gate` (task 5): at the default (or env-overridden) thresholds, assert
/// every offline-replayable corpus session's arbiter output lands within 25ms
/// of truth, the false-peak clips never take fusion, and there are zero
/// regressions vs the posterior-owns baseline (a session the baseline got
/// right that the arbiter gets wrong). Live-only sessions are excluded with a
/// printed note. Returns overall PASS/FAIL.
fn run_arb_gate(rows: &[SegmentRow], overrides: &BTreeMap<String, f64>) -> bool {
    let grouped = rows_by_session(rows);
    let t = ArbThresholds::from_env();
    println!(
        "\n=== ARBITRATION GATE (--arb-gate; narrow<={} wide>={} r>={} sharp>={}) ===",
        t.narrow, t.wide, t.fusion_r, t.fusion_sharp
    );

    let mut excluded: Vec<(String, &'static str)> = Vec::new();
    let mut scored = 0usize;
    let mut correct = 0usize;
    let mut wrong: Vec<String> = Vec::new();
    let mut regressions: Vec<String> = Vec::new();
    let mut safety_violations: Vec<String> = Vec::new();

    for (session, srows) in &grouped {
        let false_peak = FALSE_PEAK_SESSIONS.contains(&session.as_str());
        let truth = truth_for(session, overrides);

        // HARD-SAFETY first (evaluated even for excluded sessions).
        let res = arbitrate_session(srows, truth, false_peak, t);
        if res.fusion_on_false_peak {
            safety_violations.push(format!("{session} (false-peak emitted fusion)"));
        }

        if is_live_only(session) {
            let note = corpus_entry(session).map(|e| e.note).unwrap_or("live-only");
            excluded.push((session.clone(), note));
            continue;
        }
        let Some(tr) = truth else { continue };
        scored += 1;

        // Arbiter correctness: best kept row within tolerance.
        let arb_ok = res.best_err.map(|e| e <= ARB_GATE_MS).unwrap_or(false);
        if arb_ok {
            correct += 1;
        } else {
            let e = res.best_err.map(|e| format!("{e:.1}ms")).unwrap_or_else(|| "all-dropped".into());
            wrong.push(format!("{session} (best_err={e})"));
        }

        // Zero-regression vs posterior-owns baseline: if the baseline
        // (posterior offset, dropped when conf<0.4) got this session right,
        // the arbiter must not get it wrong. Baseline correct = best kept
        // posterior row within tolerance.
        let baseline_ok = srows
            .iter()
            .filter(|r| r.conf >= 0.4)
            .map(|r| (r.post - tr).abs())
            .fold(None::<f64>, |acc, e| Some(acc.map_or(e, |a| a.min(e))))
            .map(|e| e <= ARB_GATE_MS)
            .unwrap_or(false);
        if baseline_ok && !arb_ok {
            regressions.push(session.clone());
        }
    }

    // Report.
    for (s, note) in &excluded {
        let clip = corpus_entry(s).map(|e| e.clip).unwrap_or("?");
        println!("  EXCLUDED (live-only) {s} [{clip}]: {note}");
    }
    println!("  scored sessions (offline-replayable): {scored}");
    println!("  within-{ARB_GATE_MS}ms of truth: {correct}/{scored}");
    if !wrong.is_empty() {
        println!("  MISSES: {}", wrong.join("; "));
    }
    let safety_ok = safety_violations.is_empty();
    println!(
        "  HARD-SAFETY (false-peak never fusion): {}{}",
        if safety_ok { "PASS" } else { "FAIL" },
        if safety_ok { String::new() } else { format!(" — {}", safety_violations.join("; ")) }
    );
    let no_regress = regressions.is_empty();
    println!(
        "  ZERO-REGRESSION vs posterior-owns baseline: {}{}",
        if no_regress { "PASS" } else { "FAIL" },
        if no_regress { String::new() } else { format!(" — regressed: {}", regressions.join(", ")) }
    );

    let correctness_ok = scored > 0 && correct == scored;
    let pass = correctness_ok && safety_ok && no_regress;
    println!("\nARBITRATION GATE {}", if pass { "PASS" } else { "FAIL" });
    pass
}
