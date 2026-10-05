// SPDX-License-Identifier: GPL-3.0-or-later

use super::{
    synthetic::{self, Config, Scene},
    translation::{PairPoint, TranslationSolver, TranslationSolverConfig},
};

fn scenarios() -> Vec<Config> {
    let tele = Config { gyro_err_px: 0.5, ..Default::default() };
    let plane = Scene::Plane { height: 1.5, depression: (1.5f64 / 60.0).atan() };
    let drone = Config {
        f: 2000.0, scene: Scene::Plane { height: 60.0, depression: 0.52 },
        rot_sd: 1e-3, trans: [0.0, 0.0, -0.167], life: 40.0, ..tele
    };
    let walk = Config {
        f: 2000.0, scene: Scene::Volume { near: 2.0, far: 20.0 },
        rot_sd: 1e-3, trans: [0.0, 0.0, -0.025], trans_sd: 0.003, life: 40.0, ..tele
    };
    vec![
        Config { name: "tele-rigid", scene: plane, ..tele },
        Config { name: "tele-glints", scene: plane, nonrigid_px: 0.5, ..tele },
        Config { name: "tele-current", scene: plane, drift: [0.005, 0.0, 0.0], ..tele },
        Config { name: "mid-plane-glints", f: 2000.0,
            scene: Scene::Plane { height: 1.5, depression: 0.1 },
            rot_sd: 1e-3, nonrigid_px: 0.5, ..tele },
        Config { name: "wide-sea-glints", f: 600.0,
            scene: Scene::Plane { height: 3.0, depression: 0.15 },
            rot_sd: 2e-3, nonrigid_px: 0.5, life: 40.0, ..tele },
        Config { name: "mid-drone-forward", ..drone },
        Config { name: "walk-volume-2000", ..walk },
        Config { name: "tele-is", scene: plane, is_px: 5.0, is_hz: 1.5, ..tele },
        Config { name: "tele-rigid-exact", scene: plane, gyro_err_px: 0.0, ..tele },
        Config { name: "mid-drone-forward-exact", gyro_err_px: 0.0, ..drone },
        Config { name: "walk-volume-2000-exact", gyro_err_px: 0.0, ..walk },
    ]
}

fn finite(values: impl IntoIterator<Item = f64>) {
    for value in values { assert!(value.is_finite(), "non-finite measurement: {value}"); }
}

fn quantile(values: &[f64], q: f64) -> Option<f64> {
    finite(values.iter().copied());
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.is_empty() { None }
    else { Some(sorted[((sorted.len() - 1) as f64 * q).floor() as usize]) }
}

fn shown(value: Option<f64>) -> String {
    match value {
        Some(value) => { finite([value]); format!("{value:.9}") }
        None => "unavailable".to_owned(),
    }
}

#[test]
#[ignore]
fn translation_stress_bench() {
    let mut text = String::from(
        "# seed=1 per cell; frames=1140; quantiles=sorted floor((n-1)*q); unavailable=no samples or undefined
         # roll_prior_px=0.5; pred_max=0.8; exact sets gyro_err_px=0 only, noise_px remains 0.2
         yaw_pitch_prior_px	scene	pairs	confident_n	confident	claim_med	claim_p95	claim_max	true_n	true_med	ratio_n	ratio_med	leverage_n	leverage_undefined	leverage_med	on_switches	segments	pred_n	pred_q10	pred_q50	pred_q90
",
    );
    for prior in [TranslationSolverConfig::DEFAULT.yaw_pitch_prior_px, 0.5, 0.2, 0.1] {
        let config = TranslationSolverConfig { yaw_pitch_prior_px: prior, ..TranslationSolverConfig::DEFAULT };
        for c in scenarios() {
            let mut solver = TranslationSolver::new(config);
            let pairs = synthetic::generate(&c);
            let px = 1.0 / c.f;
            let translating = c.trans.iter().any(|v| *v != 0.0) || c.trans_sd > 0.0 || c.bob_m > 0.0;
            let (mut claims, mut truths, mut ratios, mut leverage, mut predictions) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let (mut on_switches, mut segments, mut leverage_undefined) = (0usize, 0usize, 0usize);
            let mut was_confident = false;
            for pair in &pairs {
                finite(pair.m_true.iter().chain(pair.m_gyro.iter()).copied());
                finite(pair.c_true.iter().copied());
                finite(pair.a.iter().chain(&pair.b).flat_map(|v| v.iter().copied()));
                finite(pair.rows.iter().map(|v| *v as f64));
                finite(pair.inv_depth.iter().copied());
                let q10 = quantile(&pair.inv_depth, 0.1).unwrap();
                let q90 = quantile(&pair.inv_depth, 0.9).unwrap();
                if q90 > q10 {
                    let value = q10 / (q90 - q10);
                    finite([value]); leverage.push(value);
                } else { leverage_undefined += 1; }
                let true_px = if translating {
                    let value = pair.c_true.norm() * quantile(&pair.inv_depth, 0.5).unwrap() / px;
                    finite([value]); truths.push(value); Some(value)
                } else { None };
                let pts: Vec<_> = pair.ids.iter().enumerate().map(|(i, id)| {
                    let p = pair.m_gyro * pair.a[i];
                    PairPoint {
                        id: *id, band: (pair.rows[i] as f64 / c.h * 6.0).floor().clamp(0.0, 5.0) as u8,
                        p, r: pair.b[i] - p,
                    }
                }).collect();
                finite(pts.iter().flat_map(|p| p.p.iter().chain(p.r.iter()).copied()));
                let result = solver.step(&pts, px, pair.index as f64 / c.fps);
                finite(result.c.iter().chain(result.c_segment.iter()).copied());
                finite(result.inv_depth.values().copied());
                finite([result.ref_inv_depth, result.confidence, result.track_age_s]);
                if let Some(pred) = result.pred { finite([pred]); predictions.push(pred); }
                segments += usize::from(result.new_segment);
                let confident = result.confidence > 0.0;
                on_switches += usize::from(confident && !was_confident);
                was_confident = confident;
                if confident {
                    let claim = result.c_segment.norm() * result.ref_inv_depth / px;
                    finite([claim]); claims.push(claim);
                    if let Some(truth) = true_px.filter(|v| *v > 0.0) {
                        let ratio = claim / truth;
                        finite([ratio]); ratios.push(ratio);
                    }
                }
            }
            let confident = if pairs.is_empty() { None } else { Some(claims.len() as f64 / pairs.len() as f64) };
            let values = [
                confident, quantile(&claims, 0.5), quantile(&claims, 0.95), quantile(&claims, 1.0),
                quantile(&truths, 0.5), quantile(&ratios, 0.5), quantile(&leverage, 0.5),
                quantile(&predictions, 0.1), quantile(&predictions, 0.5), quantile(&predictions, 0.9),
            ];
            finite(values.iter().flatten().copied());
            let line = format!(
                "{prior:.2}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}	{}
",
                c.name, pairs.len(), claims.len(), shown(values[0]), shown(values[1]), shown(values[2]), shown(values[3]),
                truths.len(), shown(values[4]), ratios.len(), shown(values[5]), leverage.len(), leverage_undefined,
                shown(values[6]), on_switches, segments, predictions.len(), shown(values[7]), shown(values[8]), shown(values[9]),
            );
            print!("{line}"); text.push_str(&line);
        }
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/optical-base/stress-bench.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
