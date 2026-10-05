// SPDX-License-Identifier: GPL-3.0-or-later

use nalgebra::{Matrix3, Rotation3, UnitQuaternion, Vector3};

struct Rng(u64);
impl Rng {
    fn u(&mut self) -> f64 {
        self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn n(&mut self) -> f64 {
        let (u1, u2) = (self.u().max(1e-300), self.u());
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Scene {
    /// Points at distances uniform in [near, far] along random rays
    Volume { near: f64, far: f64 },
    /// A horizontal plane `height` below the camera, viewed at `depression` radians down at the image center
    Plane { height: f64, depression: f64 },
}

#[derive(Clone, Copy)]
pub(crate) struct Config {
    pub name: &'static str,
    pub f: f64,
    pub w: f64,
    pub h: f64,
    pub frames: usize,
    pub points: usize,
    pub life: f64,
    pub noise_px: f64,
    pub scene: Scene,
    /// Angular velocity: AR(1) per frame, stationary sd (rad/frame) and correlation
    pub rot_sd: f64,
    pub rot_corr: f64,
    /// Camera translation per frame: constant part (world, metres) and AR(1) shake sd
    pub trans: [f64; 3],
    pub trans_sd: f64,
    /// Per-track independent motion in the image (px/frame), AR(1) with this correlation
    pub nonrigid_px: f64,
    pub nonrigid_corr: f64,
    /// Fraction of the tracks that move on their own
    pub nonrigid_frac: f64,
    /// Uniform drift of the whole textured surface (world, metres per frame)
    pub drift: [f64; 3],
    pub seed: u64,
    /// Periodic translation along the world's up axis: amplitude (metres) and frequency (Hz at 59.94 fps)
    pub bob_m: f64,
    pub bob_hz: f64,
    /// A scene that changes: before this frame the tracks move on their own and the camera doesn't translate, from
    /// it on the tracks hold still and the camera translates (0 = both all along)
    pub switch: usize,
    pub gyro_err_px: f64,
    pub is_px: f64,
    pub is_hz: f64,
    pub fps: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "", f: 15509.6, w: 960.0, h: 540.0, frames: 1140, points: 300, life: 60.0, noise_px: 0.2,
            scene: Scene::Volume { near: 80.0, far: 120.0 },
            rot_sd: 3.5e-4, rot_corr: 0.9, trans: [0.0; 3], trans_sd: 0.0,
            nonrigid_px: 0.0, nonrigid_corr: 0.9, nonrigid_frac: 1.0, drift: [0.0; 3], seed: 1,
            bob_m: 0.0, bob_hz: 0.0, switch: 0,
            gyro_err_px: 0.0, is_px: 0.0, is_hz: 0.0, fps: 59.94,
        }
    }
}

struct Pt { id: u32, world: Vector3<f64>, life: f64, own_px: Vector3<f64>, moves: bool }

pub(crate) struct Pair {
    pub index: usize,
    pub ids: Vec<u32>,
    pub a: Vec<Vector3<f64>>,
    pub b: Vec<Vector3<f64>>,
    pub rows: Vec<f32>,
    pub inv_depth: Vec<f64>,
    pub m_true: Matrix3<f64>,
    pub m_gyro: Matrix3<f64>,
    pub c_true: Vector3<f64>,
}

pub(crate) fn generate(c: &Config) -> Vec<Pair> {
    let mut rng = Rng(c.seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    // Camera pose: world -> camera rotation, and position. Camera looks along -z, y up
    let tilt = match c.scene { Scene::Plane { depression, .. } => depression, _ => 0.0 };
    let mut rot = Rotation3::from_axis_angle(&Vector3::x_axis(), tilt);
    let rot_ref = rot;
    let mut pos = Vector3::zeros();
    let mut omega = Vector3::zeros();
    let mut tshake = Vector3::zeros();
    let mut pts: Vec<Pt> = Vec::new();
    let mut next_id = 0u32;
    let ar = |rng: &mut Rng, x: f64, sd: f64, corr: f64| corr * x + sd * (1.0 - corr * corr).sqrt() * rng.n();

    let project = |rot: &Rotation3<f64>, pos: &Vector3<f64>, p: &Vector3<f64>| -> Option<(f64, f64, Vector3<f64>)> {
        let q = rot * (p - pos);
        if q.z >= -1e-6 { return None; }
        let (u, v) = (c.f * q.x / -q.z, c.f * q.y / -q.z);
        if u.abs() > c.w / 2.0 - 4.0 || v.abs() > c.h / 2.0 - 4.0 { return None; }
        Some((u, v, q))
    };
    let spawn = |rng: &mut Rng, rot: &Rotation3<f64>, pos: &Vector3<f64>, id: u32| -> Option<Pt> {
        let (u, v) = ((rng.u() - 0.5) * (c.w - 16.0), (rng.u() - 0.5) * (c.h - 16.0));
        let ray_cam = Vector3::new(u / c.f, v / c.f, -1.0).normalize();
        let ray = rot.inverse() * ray_cam;
        let dist = match c.scene {
            Scene::Volume { near, far } => near + (far - near) * rng.u(),
            Scene::Plane { height, .. } => { if ray.y >= -1e-9 { return None; } (pos.y + height) / -ray.y }
        };
        let world = pos + ray * dist;
        let life = -c.life * rng.u().max(1e-9).ln();
        let moves = rng.u() < c.nonrigid_frac;
        Some(Pt { id, world, life, own_px: Vector3::zeros(), moves })
    };

    let mut pairs = Vec::new();
    let mut prev: Option<(Rotation3<f64>, Vector3<f64>, Vec<(u32, Vector3<f64>, [f64; 2], f64)>)> = None;
    for k in 0..c.frames {
        // Keep the track count up
        let mut guard = 0;
        while pts.len() < c.points && guard < 10 * c.points {
            guard += 1;
            if let Some(p) = spawn(&mut rng, &rot, &pos, next_id) { pts.push(p); next_id += 1; }
        }
        // Observe
        let mut obs = Vec::with_capacity(pts.len());
        let is_rotation = if c.is_px != 0.0 {
            let phase = 2.0 * std::f64::consts::PI * c.is_hz * k as f64 / c.fps;
            Some(Rotation3::from_scaled_axis(Vector3::new(
                c.is_px * phase.sin(), 0.6 * c.is_px * (phase + 1.0).sin(), 0.0,
            ) / c.f))
        } else { None };
        for p in &pts {
            if let Some((u, v, q)) = project(&rot, &pos, &p.world) {
                let (un, vn) = (u + c.noise_px * rng.n(), v + c.noise_px * rng.n());
                let b = Vector3::new(un / c.f, vn / c.f, -1.0).normalize();
                let b = is_rotation.as_ref().map_or(b, |r| r * b);
                obs.push((p.id, b, [un, vn], 1.0 / q.norm()));
            }
        }
        if let Some((prot, ppos, pobs)) = prev.take() {
            let m = (rot * prot.inverse()).into_inner();
            let map: std::collections::HashMap<u32, Vector3<f64>> = obs.iter().map(|o| (o.0, o.1)).collect();
            let mut pr = Pair { index: k, ids: vec![], a: vec![], b: vec![], rows: vec![], inv_depth: vec![], m_true: m, m_gyro: m, c_true: prot * (pos - ppos) };
            for (id, a, pa, rho) in &pobs {
                if let Some(b) = map.get(id) {
                    pr.ids.push(*id); pr.a.push(*a); pr.b.push(*b);
                    pr.rows.push((c.h / 2.0 - pa[1]) as f32); pr.inv_depth.push(*rho);
                }
            }
            if pr.a.len() >= 25 {
                if c.gyro_err_px > 0.0 {
                    pr.m_gyro = Rotation3::from_scaled_axis(
                        Vector3::new(rng.n(), rng.n(), rng.n()) * (c.gyro_err_px / c.f),
                    ).into_inner() * m;
                }
                pairs.push(pr);
            }
        }
        prev = Some((rot, pos, obs));

        // Advance the world
        // Handheld-like: the aim is held, so the orientation is pulled back towards where it started
        let err = UnitQuaternion::from_rotation_matrix(&(rot * rot_ref.inverse())).scaled_axis();
        for i in 0..3 { omega[i] = ar(&mut rng, omega[i], c.rot_sd, c.rot_corr) - 0.02 * err[i]; tshake[i] = ar(&mut rng, tshake[i], c.trans_sd, 0.7); }
        let (moving, glinting) = if c.switch > 0 { (k >= c.switch, k < c.switch) } else { (true, true) };
        if moving {
            pos += Vector3::new(c.trans[0], c.trans[1], c.trans[2]) + rot.inverse() * tshake;
            if c.bob_m > 0.0 {
                let w = 2.0 * std::f64::consts::PI * c.bob_hz / 59.94;
                pos.y += c.bob_m * w * (w * k as f64).cos();
            }
        }
        rot = Rotation3::from_scaled_axis(omega) * rot;
        let drift = Vector3::new(c.drift[0], c.drift[1], c.drift[2]);
        let (rot_now, pos_now) = (rot, pos);
        for p in pts.iter_mut() {
            p.world += drift;
            if p.moves && c.nonrigid_px > 0.0 && glinting {
                for i in 0..2 { p.own_px[i] = ar(&mut rng, p.own_px[i], c.nonrigid_px, c.nonrigid_corr); }
                // Move it in the camera's image plane by own_px pixels at its distance
                let q = rot_now * (p.world - pos_now);
                let d = -q.z;
                let shift_cam = Vector3::new(p.own_px[0] * d / c.f, p.own_px[1] * d / c.f, 0.0);
                p.world += rot_now.inverse() * shift_cam;
            }
            p.life -= 1.0;
        }
        pts.retain(|p| p.life > 0.0 && project(&rot_now, &pos_now, &p.world).is_some());
    }
    pairs
}

pub(crate) fn scenarios() -> Vec<Config> {
    let tele = Config::default();
    // Grazing water plane: camera 1.5 m up, centre 60 m away
    let water = Scene::Plane { height: 1.5, depression: (1.5f64 / 60.0).atan() };
    vec![
        Config { name: "tele-rigid-volume", ..tele },
        Config { name: "tele-rigid-plane", scene: water, ..tele },
        Config { name: "tele-plane-glints(0.5px,c.9)", scene: water, nonrigid_px: 0.5, ..tele },
        Config { name: "tele-plane-glints(0.5px,c.98)", scene: water, nonrigid_px: 0.5, nonrigid_corr: 0.98, ..tele },
        Config { name: "tele-plane-current(0.3m/s)", scene: water, drift: [0.005, 0.0, 0.0], ..tele },
        Config { name: "tele-plane-current+glints", scene: water, drift: [0.005, 0.0, 0.0], nonrigid_px: 0.5, ..tele },
        Config { name: "tele-plane-hand-trans(1mm)", scene: water, trans_sd: 0.001, ..tele },
        Config { name: "mid(f=2000)-plane-glints", f: 2000.0, scene: Scene::Plane { height: 1.5, depression: 0.1 }, rot_sd: 1e-3, nonrigid_px: 0.5, ..tele },
        // Drone: 60 m up, 30 deg down, 10 m/s forward at 60 fps, wide lens
        Config { name: "wide-drone-forward", f: 600.0, scene: Scene::Plane { height: 60.0, depression: 0.52 }, rot_sd: 2e-3, trans: [0.0, 0.0, -0.167], life: 40.0, ..tele },
        Config { name: "wide-drone-sideways", f: 600.0, scene: Scene::Plane { height: 60.0, depression: 0.52 }, rot_sd: 2e-3, trans: [0.167, 0.0, 0.0], life: 40.0, ..tele },
        Config { name: "wide-walk-volume", f: 700.0, scene: Scene::Volume { near: 2.0, far: 20.0 }, rot_sd: 2e-3, trans: [0.0, 0.0, -0.025], trans_sd: 0.003, life: 40.0, ..tele },
        Config { name: "f=800-drone-forward", f: 800.0, scene: Scene::Plane { height: 60.0, depression: 0.52 }, rot_sd: 2e-3, trans: [0.0, 0.0, -0.167], life: 40.0, ..tele },
        Config { name: "f=1100-drone-forward", f: 1100.0, scene: Scene::Plane { height: 60.0, depression: 0.52 }, rot_sd: 1.5e-3, trans: [0.0, 0.0, -0.167], life: 40.0, ..tele },
        Config { name: "f=1500-drone-forward", f: 1500.0, scene: Scene::Plane { height: 60.0, depression: 0.52 }, rot_sd: 1e-3, trans: [0.0, 0.0, -0.167], life: 40.0, ..tele },
        Config { name: "f=1100-walk-volume", f: 1100.0, scene: Scene::Volume { near: 2.0, far: 20.0 }, rot_sd: 2e-3, trans: [0.0, 0.0, -0.025], trans_sd: 0.003, life: 40.0, ..tele },
        Config { name: "f=2000-walk-volume", f: 2000.0, scene: Scene::Volume { near: 2.0, far: 20.0 }, rot_sd: 1e-3, trans: [0.0, 0.0, -0.025], trans_sd: 0.003, life: 40.0, ..tele },
        Config { name: "f=600-sea-glints", f: 600.0, scene: Scene::Plane { height: 3.0, depression: 0.15 }, rot_sd: 2e-3, nonrigid_px: 0.5, life: 40.0, ..tele },
        Config { name: "f=1100-sea-glints", f: 1100.0, scene: Scene::Plane { height: 3.0, depression: 0.1 }, rot_sd: 1.5e-3, nonrigid_px: 0.5, life: 40.0, ..tele },
        Config { name: "mid(f=2000)-drone-forward", f: 2000.0, scene: Scene::Plane { height: 60.0, depression: 0.52 }, rot_sd: 1e-3, trans: [0.0, 0.0, -0.167], life: 40.0, ..tele },
        // Additional scenes cover reversing translation (walking bob and handheld sway)
        // and tracks that move independently before becoming a rigid scene.
        Config { name: "x-walk-bob(f=700)", f: 700.0, scene: Scene::Volume { near: 2.0, far: 20.0 }, rot_sd: 2e-3, trans: [0.0, 0.0, -0.025], trans_sd: 0.003, life: 40.0, bob_m: 0.02, bob_hz: 2.0, ..tele },
        Config { name: "x-walk-bob(f=2000)", f: 2000.0, scene: Scene::Volume { near: 2.0, far: 20.0 }, rot_sd: 1e-3, trans: [0.0, 0.0, -0.025], trans_sd: 0.003, life: 40.0, bob_m: 0.02, bob_hz: 2.0, ..tele },
        Config { name: "x-closeup-sway(f=1100)", f: 1100.0, scene: Scene::Volume { near: 0.5, far: 3.0 }, rot_sd: 2e-3, life: 40.0, bob_m: 0.005, bob_hz: 2.0, ..tele },
        Config { name: "x-glints-then-walk(f=700)", f: 700.0, scene: Scene::Volume { near: 2.0, far: 20.0 }, rot_sd: 2e-3, trans: [0.0, 0.0, -0.025], trans_sd: 0.003, life: 40.0, nonrigid_px: 0.5, switch: 570, ..tele },
        Config { name: "x-glints-then-walk(f=2000)", f: 2000.0, scene: Scene::Volume { near: 2.0, far: 20.0 }, rot_sd: 1e-3, trans: [0.0, 0.0, -0.025], trans_sd: 0.003, life: 40.0, nonrigid_px: 0.5, switch: 570, ..tele },
    ]
}


pub(crate) struct Jitter { pub xy_px: f64, pub all_px: f64, pub max_px: f64 }

/// Accumulated pair errors after removing a centered 31-pair moving average.
pub(crate) fn hp_jitter(est: &[Matrix3<f64>], truth: &[Matrix3<f64>], f: f64) -> Jitter {
    assert_eq!(est.len(), truth.len());
    let mut acc = Vector3::zeros();
    let traj: Vec<Vector3<f64>> = est.iter().zip(truth).map(|(est, truth)| {
        acc += UnitQuaternion::from_rotation_matrix(&Rotation3::from_matrix_unchecked(est.transpose() * truth)).scaled_axis();
        acc
    }).collect();
    let (mut xy, mut all, mut max, mut count) = (0.0, 0.0, 0.0f64, 0usize);
    for k in 15..traj.len().saturating_sub(15) {
        let mean = traj[k - 15..=k + 15].iter().sum::<Vector3<f64>>() / 31.0;
        let delta = traj[k] - mean;
        xy += delta.xy().norm_squared(); all += delta.norm_squared(); max = max.max(delta.norm()); count += 1;
    }
    Jitter { xy_px: (xy / count.max(1) as f64).sqrt() * f, all_px: (all / count.max(1) as f64).sqrt() * f, max_px: max * f }
}

#[test]
#[ignore]
fn optical_base_bench() {
    use super::{base::VisionBase, OpticalBaseMode, BlendConfig, robust_rotation};
    let modes = [OpticalBaseMode::Rotation, OpticalBaseMode::Odometry, OpticalBaseMode::Auto];
    let mut results = Vec::new();
    for c in scenarios() {
        let mut values = [[0.0f64; 3]; 3];
        for seed in 1..=3 {
            let cfg = Config { seed, ..c }; let pairs = generate(&cfg);
            let truth: Vec<_> = pairs.iter().map(|p| p.m_true).collect();
            let m0s: Vec<_> = pairs.iter().map(|p| robust_rotation(&p.a, &p.b)).collect();
            for (mode_index, mode) in modes.iter().enumerate() {
                let mut base = VisionBase::new(*mode, BlendConfig::DEFAULT);
                let mut previous = None;
                let est: Vec<_> = pairs.iter().zip(&m0s).map(|(p,m0)| {
                    if previous.is_some_and(|i| p.index != i + 1) { base.reset(); }
                    previous = Some(p.index);
                    base.rotation(p.index, &p.a, &p.b, &p.ids, *m0, 1.0 / cfg.f)
                }).collect();
                let j = hp_jitter(&est, &truth, cfg.f);
                assert!([j.xy_px,j.all_px,j.max_px].iter().all(|v| v.is_finite()), "{} seed={seed} mode={mode:?}", cfg.name);
                values[mode_index][0] += j.xy_px / 3.0; values[mode_index][1] += j.all_px / 3.0; values[mode_index][2] = values[mode_index][2].max(j.max_px);
            }
        }
        results.push((c.name,values));
        eprintln!("completed {}",c.name);
    }
    let mut text = String::new();
    for (metric_index,metric) in ["xy","all","max"].iter().enumerate() {
        text.push_str(&format!("[{}]\nscene rotation odometry auto\n",metric));
        for (name, values) in &results { text.push_str(&format!("{name} {:.9} {:.9} {:.9}\n",values[0][metric_index],values[1][metric_index],values[2][metric_index])); }
        text.push('\n');
    }
    print!("{text}");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/optical-base/synthetic-bench.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap(); std::fs::write(path,text).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gyro_error_has_the_requested_size() {
        let c = Config { gyro_err_px: 0.5, frames: 1000, ..Default::default() };
        let pairs = generate(&c);
        assert!(!pairs.is_empty());
        let mean = pairs.iter().map(|p| {
            UnitQuaternion::from_rotation_matrix(&Rotation3::from_matrix_unchecked(p.m_gyro * p.m_true.transpose()))
                .scaled_axis().norm_squared() / 3.0
        }).sum::<f64>() / pairs.len() as f64;
        let size = c.f * mean.sqrt();
        assert!((size - 0.5).abs() <= 0.05, "size={size}");
    }
    #[test]
    fn is_shift_is_invisible_to_the_gyro() {
        let c = Config { is_px: 5.0, is_hz: 1.5, gyro_err_px: 0.0, noise_px: 0.0, frames: 120, ..Default::default() };
        let mut shifts = Vec::new();
        for p in generate(&c) {
            assert_eq!(p.m_gyro, p.m_true);
            let mut residuals: Vec<_> = p.a.iter().zip(&p.b).map(|(a, b)| (b - p.m_gyro * a).norm()).collect();
            residuals.sort_by(f64::total_cmp);
            shifts.push(c.f * residuals[((residuals.len() - 1) as f64 * 0.5).floor() as usize]);
        }
        shifts.sort_by(f64::total_cmp);
        let median = shifts[((shifts.len() - 1) as f64 * 0.5).floor() as usize];
        assert!(median > 0.3, "median={median}");
    }
    #[test]
    fn noise_free_rigid_pairs_are_exact_rotations() {
        let c = Config { noise_px: 0.0, frames: 50, ..Default::default() };
        for p in generate(&c) { for (a,b) in p.a.iter().zip(&p.b) { assert!((b-p.m_true*a).norm() < 1e-9); } }
    }
    #[test]
    fn translation_moves_near_points_more() {
        let c = Config { scene: Scene::Volume {near:2.0,far:20.0}, f:700.0, rot_sd:0.0, trans:[0.03,0.0,0.0], noise_px:0.0, frames:20, ..Default::default() };
        for p in generate(&c) {
            let mut depths = p.inv_depth.clone(); depths.sort_by(f64::total_cmp); let median=depths[depths.len()/2];
            let (mut near,mut far) = ((0.0,0usize),(0.0,0usize));
            for ((a,b),rho) in p.a.iter().zip(&p.b).zip(&p.inv_depth) { let dst=(b-p.m_true*a).norm(); let group=if *rho>median {&mut near} else {&mut far}; group.0+=dst; group.1+=1; }
            assert!(near.0/near.1 as f64 > 2.0*far.0/far.1 as f64);
        }
    }
    #[test]
    fn switch_holds_the_camera_until_its_frame() {
        let c = Config { scene: Scene::Volume {near:2.0,far:20.0}, f:700.0, rot_sd:0.0, noise_px:0.0, nonrigid_px:0.5, trans:[0.02,0.0,0.0], frames:60, switch:30, ..Default::default() };
        for p in generate(&c) { if p.index<=29 { assert!(p.c_true.norm()<1e-12); } if p.index>=31 { assert!((p.c_true.norm()-0.02).abs()<1e-9); } }
    }
    #[test]
    fn bob_moves_the_camera_up_and_down() {
        let c = Config { scene: Scene::Volume {near:2.0,far:20.0}, f:700.0, rot_sd:0.0, noise_px:0.0, bob_m:0.02, bob_hz:2.0, frames:62, ..Default::default() };
        let (mut sum,mut min,mut max)=(0.0f64,0.0f64,0.0f64);
        for p in generate(&c) { sum+=p.c_true.y; min=min.min(sum); max=max.max(sum); }
        assert!(((max-min)-0.04).abs()<=0.004,"span={}",max-min);
    }
}
