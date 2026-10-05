// SPDX-License-Identifier: GPL-3.0-or-later

//! Continuous trust in the odometry's extrapolation from visible depths to infinity.

use std::{fs::{File, OpenOptions}, io::Write, path::PathBuf, sync::{OnceLock, atomic::{AtomicUsize, Ordering}}};
use nalgebra::{Matrix3, Rotation3, UnitQuaternion, Vector3};
use super::odometry::{VisualOdometry, StepDiagnostics};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpticalBaseMode { Auto, Rotation, Odometry }

impl OpticalBaseMode {
    pub fn as_str(self) -> &'static str { match self { Self::Auto => "auto", Self::Rotation => "rotation", Self::Odometry => "odometry" } }
    pub fn resolved() -> Self {
        static RESOLVED: OnceLock<OpticalBaseMode> = OnceLock::new();
        *RESOLVED.get_or_init(|| {
            let raw = std::env::var("GYROFLOW_OPTICAL_BASE").ok();
            let mode = raw.as_deref().and_then(parse_mode).unwrap_or(Self::Auto);
            if raw.is_some() && raw.as_deref().and_then(parse_mode).is_none() {
                log::warn!(target: "lifecycle", "GYROFLOW_OPTICAL_BASE={} invalid, falling back to auto", raw.as_deref().unwrap());
            }
            log::info!(target: "lifecycle", "optical_base_config resolved={} source={}", mode.as_str(), if raw.is_some() { "env" } else { "default" });
            mode
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlendConfig { pub ratio_full: f64, pub ratio_none: f64, pub leverage: f64 }

impl BlendConfig {
    pub const DEFAULT: Self = Self { ratio_full: 0.55, ratio_none: 0.70, leverage: 1.0 };
    pub fn resolved() -> Self {
        static RESOLVED: OnceLock<BlendConfig> = OnceLock::new();
        *RESOLVED.get_or_init(|| {
            let full = std::env::var("GYROFLOW_OPTICAL_BASE_RATIO_FULL").ok();
            let none = std::env::var("GYROFLOW_OPTICAL_BASE_RATIO_NONE").ok();
            let leverage = std::env::var("GYROFLOW_OPTICAL_BASE_LEVERAGE").ok();
            let mut config = Self {
                ratio_full: resolve_number("GYROFLOW_OPTICAL_BASE_RATIO_FULL", full.as_deref(), Self::DEFAULT.ratio_full, parse_ratio),
                ratio_none: resolve_number("GYROFLOW_OPTICAL_BASE_RATIO_NONE", none.as_deref(), Self::DEFAULT.ratio_none, parse_ratio),
                leverage: resolve_number("GYROFLOW_OPTICAL_BASE_LEVERAGE", leverage.as_deref(), Self::DEFAULT.leverage, parse_leverage),
            };
            if checked(config).is_none() {
                log::warn!(target: "lifecycle", "optical_base ratios full={} none={} invalid, falling back to {}/{}", config.ratio_full, config.ratio_none, Self::DEFAULT.ratio_full, Self::DEFAULT.ratio_none);
                config.ratio_full = Self::DEFAULT.ratio_full; config.ratio_none = Self::DEFAULT.ratio_none;
            }
            for (name,value,raw) in [("optical_base_ratio_full", config.ratio_full, full), ("optical_base_ratio_none", config.ratio_none, none), ("optical_base_leverage", config.leverage, leverage)] {
                log::info!(target: "lifecycle", "{}_config resolved={} source={}", name, value, if raw.is_some() { "env" } else { "default" });
            }
            config
        })
    }
}

fn resolve_number(name: &str, raw: Option<&str>, default: f64, parse: fn(&str) -> Option<f64>) -> f64 {
    match raw { Some(raw) => match parse(raw) {
        Some(value) => value,
        None => { log::warn!(target: "lifecycle", "{}={} invalid, falling back to {}", name, raw, default); default },
    }, None => default }
}
pub(super) fn parse_mode(raw: &str) -> Option<OpticalBaseMode> {
    match raw.trim().to_ascii_lowercase().as_str() { "auto" => Some(OpticalBaseMode::Auto), "rotation" => Some(OpticalBaseMode::Rotation), "odometry" => Some(OpticalBaseMode::Odometry), _ => None }
}
pub(super) fn parse_ratio(raw: &str) -> Option<f64> { raw.trim().parse::<f64>().ok().filter(|x| x.is_finite() && *x > 0.0 && *x <= 2.0) }
pub(super) fn parse_leverage(raw: &str) -> Option<f64> { raw.trim().parse::<f64>().ok().filter(|x| x.is_finite() && *x > 0.0 && *x <= 100.0) }
pub(super) fn checked(config: BlendConfig) -> Option<BlendConfig> { (config.ratio_full < config.ratio_none).then_some(config) }

/// The trust requested by this pair, before smoothing over pairs.
pub(super) fn weight_target(ratio: f64, far: f64, near: f64, config: &BlendConfig) -> f64 {
    if ![ratio,far,near].iter().all(|x| x.is_finite()) { return 0.0; }
    let structure = ((config.ratio_none - ratio) / (config.ratio_none - config.ratio_full)).clamp(0.0, 1.0);
    let leverage = far / (near - far).max(1e-300);
    let extrapolation = 1.0 / (1.0 + (leverage / config.leverage).powi(4));
    structure * extrapolation
}

fn rotvec(m: Matrix3<f64>) -> Vector3<f64> {
    UnitQuaternion::from_rotation_matrix(&Rotation3::from_matrix_unchecked(m)).scaled_axis()
}

struct Dump { path: PathBuf, attempted: bool, file: Option<File> }
impl Dump {
    fn write(&mut self, row: &str) {
        if !self.attempted {
            self.attempted = true;
            match OpenOptions::new().create(true).append(true).open(&self.path) {
                Ok(mut file) => {
                    if file.metadata().map(|m| m.len() == 0).unwrap_or(false) {
                        if let Err(error) = writeln!(file, "run,index,event,structure,ratio,far,near,weight,m0x,m0y,m0z,mx,my,mz") {
                            log::warn!("Optical base dump {}: {}", self.path.display(), error); return;
                        }
                    }
                    self.file = Some(file);
                },
                Err(error) => log::warn!("Optical base dump {}: {}", self.path.display(), error),
            }
        }
        if let Some(file) = self.file.as_mut() {
            if let Err(error) = writeln!(file,"{row}") { log::warn!("Optical base dump {}: {}", self.path.display(), error); self.file = None; }
        }
    }
}

pub(super) struct VisionBase {
    mode: OpticalBaseMode,
    config: BlendConfig,
    odometry: VisualOdometry,
    ratio: Option<f64>,
    weight: f64,
    pairs: usize,
    restart: usize,
    invalid: usize,
    resets: usize,
    weights: Vec<f64>,
    structures: Vec<f64>,
    leverages: Vec<f64>,
    run: usize,
    dump: Option<Dump>,
}

impl VisionBase {
    pub(super) fn new(mode: OpticalBaseMode, config: BlendConfig) -> Self {
        static RUN: AtomicUsize = AtomicUsize::new(0);
        Self { mode, config, odometry: VisualOdometry::default(), ratio: None, weight: 0.0, pairs: 0, restart: 0, invalid: 0, resets: 0,
            weights: Vec::new(), structures: Vec::new(), leverages: Vec::new(), run: RUN.fetch_add(1, Ordering::Relaxed) + 1,
            dump: std::env::var_os("GYROFLOW_OPTICAL_BASE_DUMP").map(|path| Dump { path: path.into(), attempted: false, file: None }) }
    }

    pub(super) fn rotation(&mut self, index: usize, a: &[Vector3<f64>], b: &[Vector3<f64>], ids: &[u32], m0: Matrix3<f64>, px: f64) -> Matrix3<f64> {
        let (m, diagnostics) = if self.mode == OpticalBaseMode::Rotation { (m0, None) } else {
            let (m,d) = self.odometry.step_with_diagnostics(a,b,ids,m0,px); (m,Some(d))
        };
        self.pairs += 1;
        if let Some(d) = diagnostics {
            if d.restarted { self.restart += 1; } else {
                if let Some(structure) = d.structure { self.structures.push(structure); }
                self.leverages.push(d.far / (d.near - d.far).max(1e-300));
            }
        }
        let (output,event,pair_ratio) = match self.mode {
            OpticalBaseMode::Rotation => (m0,"rotation",None),
            OpticalBaseMode::Odometry => (m,"odometry",None),
            OpticalBaseMode::Auto => self.blend(m0,m,diagnostics.unwrap()),
        };
        if self.mode != OpticalBaseMode::Odometry { self.weights.push(self.weight); }
        if let Some(dump) = self.dump.as_mut() {
            let d = diagnostics;
            let structure = d.and_then(|d|d.structure).map(|v|v.to_string()).unwrap_or_default();
            let far = d.map(|d|d.far.to_string()).unwrap_or_default(); let near = d.map(|d|d.near.to_string()).unwrap_or_default();
            let ratio = if self.mode == OpticalBaseMode::Auto { pair_ratio.map(|v|v.to_string()).unwrap_or_default() } else { String::new() };
            let weight = if self.mode == OpticalBaseMode::Auto { self.weight.to_string() } else { String::new() };
            let (r0,r) = (rotvec(m0),rotvec(m));
            dump.write(&format!("{},{},{},{},{},{},{},{},{},{},{},{},{},{}",self.run,index,event,structure,ratio,far,near,weight,r0.x,r0.y,r0.z,r.x,r.y,r.z));
        }
        output
    }

    fn blend(&mut self, m0: Matrix3<f64>, m: Matrix3<f64>, d: StepDiagnostics) -> (Matrix3<f64>, &'static str, Option<f64>) {
        if d.restarted { self.ratio = None; self.weight = 0.0; return (m0,"restart",None); }
        let Some(structure) = d.structure.filter(|x|x.is_finite()) else { self.weight = 0.0; self.invalid += 1; return (m0,"invalid",self.ratio); };
        let ratio = 0.8 * self.ratio.unwrap_or(self.config.ratio_full) + 0.2 * structure;
        self.ratio = Some(ratio);
        let target = weight_target(ratio,d.far,d.near,&self.config);
        self.weight = target.min(0.8 * self.weight + 0.2 * target);
        let event = if ratio > self.config.ratio_none {
            self.odometry.reset(); self.ratio = None; self.resets += 1; "reset"
        } else { "blend" };
        let output = if self.weight == 0.0 { m0 } else { Rotation3::from_scaled_axis(rotvec(m * m0.transpose()) * self.weight).into_inner() * m0 };
        (output,event,Some(ratio))
    }

    pub(super) fn reset(&mut self) { self.odometry.reset(); self.ratio = None; self.weight = 0.0; }
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn weight(&self) -> f64 { self.weight }
    pub(super) fn summary(&self) -> String {
        let odometry = self.mode == OpticalBaseMode::Odometry;
        format!("mode={} pairs={} restart={} invalid={} reset={} weight_q10/50/90={} structure_q10/50/90={} leverage_q10/50/90={}",
            self.mode.as_str(),self.pairs,self.restart,if odometry { "-".into() } else { self.invalid.to_string() },if odometry { "-".into() } else { self.resets.to_string() },
            if odometry { "-".into() } else { quantiles(&self.weights) },quantiles(&self.structures),quantiles(&self.leverages))
    }
}

fn quantiles(values: &[f64]) -> String {
    if values.is_empty() { return "-".into(); }
    let mut sorted = values.to_vec(); sorted.sort_by(f64::total_cmp);
    let q = |fraction: f64| sorted[((sorted.len() - 1) as f64 * fraction) as usize];
    format!("{:.6}/{:.6}/{:.6}",q(0.1),q(0.5),q(0.9))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{robust_rotation, synthetic::{self, Config, Pair}};
    fn scene(name: &str) -> Config { synthetic::scenarios().into_iter().find(|c| c.name == name).unwrap() }
    fn outputs(pairs: &[Pair], f: f64, mode: OpticalBaseMode) -> Vec<Matrix3<f64>> {
        let mut base = VisionBase::new(mode, BlendConfig::DEFAULT);
        let mut previous = None;
        pairs.iter().map(|p| {
            if previous.is_some_and(|i| p.index != i + 1) { base.reset(); }
            previous = Some(p.index);
            base.rotation(p.index, &p.a, &p.b, &p.ids, robust_rotation(&p.a, &p.b), 1.0 / f)
        }).collect()
    }
    fn jitter(pairs: &[Pair], f: f64, mode: OpticalBaseMode) -> f64 {
        synthetic::hp_jitter(&outputs(pairs, f, mode), &pairs.iter().map(|p| p.m_true).collect::<Vec<_>>(), f).xy_px
    }
    #[test]
    fn parse_mode_accepts_the_three_modes_only() {
        for (raw, value) in [("auto", OpticalBaseMode::Auto), (" Rotation ", OpticalBaseMode::Rotation), ("odometry", OpticalBaseMode::Odometry)] { assert_eq!(parse_mode(raw), Some(value)); }
        for raw in ["m0", "", "rot"] { assert_eq!(parse_mode(raw), None); }
    }
    #[test]
    fn numbers_reject_out_of_range() {
        assert_eq!(parse_ratio("0.55"), Some(0.55));
        for raw in ["0", "-1", "2.5", "nan", "x", "", "inf"] { assert_eq!(parse_ratio(raw), None); }
        assert_eq!(parse_leverage("1"), Some(1.0)); assert_eq!(parse_leverage("100"), Some(100.0));
        for raw in ["0", "101", "-1", "nan", "inf", ""] { assert_eq!(parse_leverage(raw), None); }
        assert_eq!(checked(BlendConfig::DEFAULT), Some(BlendConfig::DEFAULT));
        for (full, none) in [(0.70, 0.55), (0.55, 0.55)] { assert_eq!(checked(BlendConfig { ratio_full: full, ratio_none: none, leverage: 1.0 }), None); }
    }
    #[test]
    fn weight_target_is_the_product_of_the_two_factors() {
        for (ratio, far, near, expected) in [(0.5,0.0,1.0,1.0),(0.625,0.0,1.0,0.5),(0.7,0.0,1.0,0.0),(0.9,0.0,1.0,0.0),(0.5,1.0,2.0,0.5),(0.5,2.0,3.0,1.0/17.0),(0.5,1.0,1.0,0.0),(0.5,0.0,0.0,1.0),(f64::NAN,0.0,1.0,0.0),(0.5,f64::INFINITY,1.0,0.0)] {
            let w = weight_target(ratio, far, near, &BlendConfig::DEFAULT);
            assert!(w.is_finite() && (0.0..=1.0).contains(&w)); assert!((w - expected).abs() < 1e-12, "{ratio} {far} {near}: {w}");
        }
    }
    #[test]
    fn diagnostics_do_not_change_the_step() {
        for name in ["tele-plane-glints(0.5px,c.9)", "wide-walk-volume"] {
            let c = scene(name); let pairs = synthetic::generate(&c);
            let (mut step, mut diagnostics) = (VisualOdometry::default(), VisualOdometry::default());
            let mut previous = None;
            for p in pairs.iter().take(60) {
                if previous.is_some_and(|i| p.index != i + 1) { step.reset(); diagnostics.reset(); }
                previous = Some(p.index);
                let m0 = robust_rotation(&p.a, &p.b);
                let a = step.step(&p.a, &p.b, &p.ids, m0, 1.0 / c.f);
                let b = diagnostics.step_with_diagnostics(&p.a, &p.b, &p.ids, m0, 1.0 / c.f).0;
                assert_eq!(a.map(f64::to_bits), b.map(f64::to_bits));
            }
        }
    }
    #[test]
    fn odometry_mode_is_the_upstream_step() {
        for name in ["tele-plane-glints(0.5px,c.9)", "wide-walk-volume"] {
            let c = scene(name); let pairs = synthetic::generate(&c);
            let mut upstream = VisualOdometry::default(); let mut base = VisionBase::new(OpticalBaseMode::Odometry, BlendConfig::DEFAULT);
            let mut previous = None;
            for p in pairs.iter().take(60) {
                if previous.is_some_and(|i| p.index != i + 1) { upstream.reset(); base.reset(); }
                previous = Some(p.index);
                let m0 = robust_rotation(&p.a, &p.b);
                assert_eq!(upstream.step(&p.a, &p.b, &p.ids, m0, 1.0/c.f).map(f64::to_bits), base.rotation(p.index, &p.a, &p.b, &p.ids, m0, 1.0/c.f).map(f64::to_bits));
            }
        }
    }
    #[test]
    fn rotation_mode_returns_m0() {
        let c = scene("wide-walk-volume"); let mut base = VisionBase::new(OpticalBaseMode::Rotation, BlendConfig::DEFAULT);
        for p in synthetic::generate(&c).iter().take(20) { let m0 = robust_rotation(&p.a, &p.b); assert_eq!(base.rotation(p.index, &p.a, &p.b, &p.ids, m0, 1.0/c.f), m0); }
    }
    #[test]
    fn weight_starts_from_zero_after_every_restart() {
        let c = scene("wide-walk-volume"); let pairs = synthetic::generate(&c); let mut base = VisionBase::new(OpticalBaseMode::Auto, BlendConfig::DEFAULT);
        for (k, p) in pairs.iter().take(61).enumerate() {
            let m0 = robust_rotation(&p.a, &p.b); let out = base.rotation(p.index, &p.a, &p.b, &p.ids, m0, 1.0/c.f);
            if k == 0 { assert_eq!(out, m0); assert_eq!(base.weight(), 0.0); }
            if (1..=10).contains(&k) { assert!(base.weight() <= 1.0 - 0.8f64.powi(k as i32) + 1e-12); }
            if k == 60 { assert!(base.weight() > 0.9, "weight={}", base.weight()); }
        }
        base.reset(); assert_eq!(base.weight(), 0.0);
        let p = &pairs[61]; let m0 = robust_rotation(&p.a, &p.b); assert_eq!(base.rotation(p.index, &p.a, &p.b, &p.ids, m0, 1.0/c.f), m0);
        // Unknown track identities restart the odometry even without an explicit reset.
        let ids: Vec<u32> = p.ids.iter().map(|id| id + 1_000_000).collect();
        assert_eq!(base.rotation(p.index + 1, &p.a, &p.b, &ids, m0, 1.0/c.f), m0); assert_eq!(base.weight(), 0.0);
    }
    #[test]
    fn auto_stays_on_m0_where_the_structure_is_fake() {
        let c = scene("tele-plane-glints(0.5px,c.9)"); let p = synthetic::generate(&c);
        let auto = jitter(&p,c.f,OpticalBaseMode::Auto); let odom = jitter(&p,c.f,OpticalBaseMode::Odometry);
        assert!(auto < 0.2, "auto={auto}"); assert!(odom > 50.0, "odometry={odom}");
    }
    #[test]
    fn auto_keeps_the_odometry_where_it_helps() {
        let c = scene("wide-walk-volume"); let p = synthetic::generate(&c);
        let auto = jitter(&p,c.f,OpticalBaseMode::Auto); let rot = jitter(&p,c.f,OpticalBaseMode::Rotation);
        assert!(auto < 0.08, "auto={auto}"); assert!(rot > 0.5, "rotation={rot}");
    }
    #[test]
    fn auto_leans_on_m0_over_a_plane_at_a_mid_focal_length() {
        let c = scene("mid(f=2000)-drone-forward"); let p = synthetic::generate(&c);
        let auto = jitter(&p,c.f,OpticalBaseMode::Auto); let odom = jitter(&p,c.f,OpticalBaseMode::Odometry);
        assert!(auto < 0.25, "auto={auto}"); assert!(odom > 1.0, "odometry={odom}");
    }
    #[test]
    fn auto_recovers_after_the_scene_turns_rigid() {
        let c = scene("x-glints-then-walk(f=2000)"); let p = synthetic::generate(&c);
        let auto = outputs(&p,c.f,OpticalBaseMode::Auto); let rot = outputs(&p,c.f,OpticalBaseMode::Rotation); let truth: Vec<_> = p.iter().map(|p|p.m_true).collect();
        let head = synthetic::hp_jitter(&auto[..500],&truth[..500],c.f).xy_px; let start = p.len()-400;
        let tail = synthetic::hp_jitter(&auto[start..],&truth[start..],c.f).xy_px; let rt = synthetic::hp_jitter(&rot[start..],&truth[start..],c.f).xy_px;
        assert!(head < 0.25, "head={head}"); assert!(tail < 0.2, "tail={tail}"); assert!(rt > 1.0, "rotation tail={rt}");
    }
    #[test]
    fn invalid_structure_and_internal_resets_revoke_the_weight() {
        let m0 = Rotation3::from_scaled_axis(Vector3::new(0.01,0.02,0.0)).into_inner();
        let m = Rotation3::from_scaled_axis(Vector3::new(0.0,0.03,0.0)).into_inner() * m0;
        let mut base = VisionBase::new(OpticalBaseMode::Auto,BlendConfig::DEFAULT);
        let d = StepDiagnostics { restarted:false, structure:Some(0.1), far:0.0, near:1.0 };
        for _ in 0..20 { base.blend(m0,m,d); }
        assert!(base.weight()>0.9);
        let ratio = base.ratio;
        for structure in [None,Some(f64::NAN),Some(f64::INFINITY)] {
            assert_eq!(base.blend(m0,m,StepDiagnostics {structure,..d}), (m0,"invalid",ratio));
            assert_eq!(base.weight(),0.0); assert_eq!(base.ratio,ratio);
        }
        assert_eq!(base.blend(m0,m,StepDiagnostics {structure:Some(5.0),..d}), (m0,"reset",Some(0.8*ratio.unwrap()+1.0)));
        assert_eq!(base.weight(),0.0); assert_eq!(base.ratio,None);
        assert_eq!(base.blend(m0,m,StepDiagnostics {restarted:true,..d}), (m0,"restart",None));
        assert_eq!(base.weight(),0.0); assert_eq!(base.ratio,None);
        // A singular solve takes the same restart path as a lost set of tracks.
        assert_eq!(base.rotation(1,&[],&[],&[],m0,1.0/700.0),m0); assert_eq!(base.weight(),0.0);
    }
    #[test]
    fn dump_opens_on_the_first_pair_and_keeps_the_run_and_mode_columns() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/optical-base/test-dumps");
        std::fs::create_dir_all(&path).unwrap();
        let c = scene("wide-walk-volume"); let pairs = synthetic::generate(&Config {frames:3,..c}); let p=&pairs[0]; let m0=robust_rotation(&p.a,&p.b);
        for mode in [OpticalBaseMode::Rotation,OpticalBaseMode::Odometry,OpticalBaseMode::Auto] {
            let mut base=VisionBase::new(mode,BlendConfig::DEFAULT);
            let file=path.join(format!("{}-{}.csv",std::process::id(),base.run));
            base.dump=Some(Dump {path:file.clone(),attempted:false,file:None});
            assert!(!file.exists());
            base.rotation(p.index,&p.a,&p.b,&p.ids,m0,1.0/c.f);
            let contents=std::fs::read_to_string(&file).unwrap(); let rows:Vec<_>=contents.lines().collect();
            assert_eq!(rows[0],"run,index,event,structure,ratio,far,near,weight,m0x,m0y,m0z,mx,my,mz");
            let cells:Vec<_>=rows[1].split(',').collect(); assert_eq!(cells.len(),14); assert_eq!(cells[0],base.run.to_string());
            match mode {
                OpticalBaseMode::Rotation=>{assert_eq!(cells[2],"rotation");assert!(cells[3..8].iter().all(|v|v.is_empty()));},
                OpticalBaseMode::Odometry=>{assert_eq!(cells[2],"odometry");assert_eq!(cells[4],"");assert_eq!(cells[7],"");assert!(base.summary().contains("weight_q10/50/90=-"));},
                OpticalBaseMode::Auto=>{assert_eq!(cells[2],"restart");assert_eq!(cells[7],"0");},
            }
        }
    }

}
