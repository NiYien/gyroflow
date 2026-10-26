// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in capture and replay of the exact filtered measurements seen by a deep scan.

use super::*;
#[cfg(test)]
use std::io::{BufReader, Read};
use std::io::{BufWriter, Write};

#[derive(serde::Serialize, serde::Deserialize)]
struct Header {
    initial: f64,
    radius: f64,
    range_index: usize,
    center_ms: f64,
    duration_ms: f64,
    observations: usize,
    gyro: usize,
}

pub(in super::super) fn capture(
    of: &[TimeIMU],
    gyro: &BTreeMap<usize, TimeIMU>,
    grid: Grid,
    index: usize,
    center_ms: f64,
    duration_ms: f64,
) {
    let Ok(dir) = std::env::var("GYROFLOW_DEEP_SCAN_CAPTURE") else {
        return;
    };
    if dir.is_empty() {
        return;
    }
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let result = (|| -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let dir = std::path::Path::new(&dir);
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!(
            "scan-{}-{}.bin",
            std::process::id(),
            SEQUENCE.fetch_add(1, Relaxed)
        ));
        let mut writer = BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?,
        );
        writer.write_all(b"GFSCAN01")?;
        let header = Header {
            initial: grid.initial,
            radius: grid.radius,
            range_index: index,
            center_ms,
            duration_ms,
            observations: of.len(),
            gyro: gyro.len(),
        };
        let json = serde_json::to_vec(&header)?;
        writer.write_all(&(json.len() as u32).to_le_bytes())?;
        writer.write_all(&json)?;
        let mut row = |timestamp: u64, rates: Option<[f64; 3]>| -> std::io::Result<()> {
            writer.write_all(&timestamp.to_le_bytes())?;
            for value in rates.unwrap_or_default() {
                writer.write_all(&value.to_le_bytes())?;
            }
            writer.write_all(&(rates.is_some() as u64).to_le_bytes())
        };
        for sample in of {
            row(sample.timestamp_ms.to_bits(), sample.gyro)?;
        }
        for (&time, sample) in gyro {
            row(time as u64, sample.gyro)?;
        }
        writer.flush()?;
        Ok(path)
    })();
    match result {
        Ok(path) => log::info!(target: "sync", "[deep-scan] captured {}", path.display()),
        Err(error) => log::warn!(target: "sync", "[deep-scan] capture failed: {error}"),
    }
}

#[cfg(test)]
fn read_capture(path: &std::path::Path) -> (Header, Vec<TimeIMU>, BTreeMap<usize, TimeIMU>) {
    let mut reader = BufReader::new(std::fs::File::open(path).unwrap());
    let mut magic = [0; 8];
    reader.read_exact(&mut magic).unwrap();
    assert_eq!(&magic, b"GFSCAN01");
    let mut length = [0; 4];
    reader.read_exact(&mut length).unwrap();
    let length = u32::from_le_bytes(length) as usize;
    assert!(length <= 65536);
    let mut json = vec![0; length];
    reader.read_exact(&mut json).unwrap();
    let header: Header = serde_json::from_slice(&json).unwrap();
    let mut row = || {
        let mut bytes = [0; 40];
        reader.read_exact(&mut bytes).unwrap();
        let time = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let values = std::array::from_fn(|i| {
            f64::from_le_bytes(bytes[8 + i * 8..16 + i * 8].try_into().unwrap())
        });
        let valid = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) != 0;
        (time, valid.then_some(values))
    };
    let of = (0..header.observations)
        .map(|_| {
            let (time, gyro) = row();
            TimeIMU {
                timestamp_ms: f64::from_bits(time),
                gyro,
                ..Default::default()
            }
        })
        .collect();
    let gyro = (0..header.gyro)
        .map(|_| {
            let (time, gyro) = row();
            (
                time as usize,
                TimeIMU {
                    timestamp_ms: time as f64 / 1000.0,
                    gyro,
                    ..Default::default()
                },
            )
        })
        .collect();
    (header, of, gyro)
}

#[cfg(test)]
fn reference_scan(of: &[TimeIMU], source: &BTreeMap<usize, TimeIMU>, grid: Grid) -> ScanResult {
    let done = AtomicUsize::new(0);
    let bucket = AtomicUsize::new(0);
    let minimum = (0..grid.steps())
        .into_par_iter()
        .map(|j| {
            let offset = grid.offset(j);
            let cost = super::super::calculate_cost(offset, of, source);
            let count = done.fetch_add(1, Relaxed) + 1;
            bucket.fetch_max(count * 100 / grid.steps().max(1), Relaxed);
            (offset, cost)
        })
        .reduce_with(|a, b| if a.1 < b.1 { a } else { b });
    let curve = (0..grid.curve_steps())
        .into_par_iter()
        .map(|j| {
            let x = grid.initial - grid.radius + j as f64 * 25.0;
            (x, super::super::calculate_cost(x, of, source))
        })
        .filter(|(_, c)| c.is_finite() && *c != f64::MAX)
        .collect();
    ScanResult {
        minimum,
        curve,
        backend: Backend::Legacy,
    }
}

#[test]
#[ignore = "requires GYROFLOW_DEEP_SCAN_REPLAY pointing to real captured measurements"]
fn deep_scan_replay() {
    use crate::synchronization::deep_match as dm;
    use std::time::Instant;
    struct ReplayLog;
    impl log::Log for ReplayLog {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            let message = record.args().to_string();
            if message.starts_with("[deep-scan]") {
                eprintln!("{message}");
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: ReplayLog = ReplayLog;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
    let root = std::path::PathBuf::from(
        std::env::var("GYROFLOW_DEEP_SCAN_REPLAY").expect("capture directory"),
    );
    let mut paths: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|s| s == "bin"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    let mut report = Vec::new();
    let mut curves = Vec::new();
    let mut clip_ms = 0.0;
    let repetitions: usize = std::env::var("GYROFLOW_DEEP_SCAN_REPEATS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let with_gpu = std::env::var("GYROFLOW_DEEP_SCAN_REPLAY_GPU").is_ok_and(|v| v == "1");
    for path in paths {
        let (header, of, source) = read_capture(&path);
        let grid = Grid {
            initial: header.initial,
            radius: header.radius,
        };
        clip_ms = header.duration_ms;
        let mut expected = None;
        for round in 0..repetitions {
            let mut order = if with_gpu {
                vec![Backend::Legacy, Backend::Cpu, Backend::Gpu]
            } else {
                vec![Backend::Legacy, Backend::Cpu]
            };
            let rotate = round % order.len();
            order.rotate_left(rotate);
            let mut runs = Vec::new();
            for backend in order {
                let t = Instant::now();
                let result = if backend == Backend::Legacy {
                    reference_scan(&of, &source, grid)
                } else {
                    scan_with_backend(
                        backend,
                        &of,
                        &source,
                        grid,
                        true,
                        0,
                        &AtomicBool::new(false),
                        &|_| {},
                    )
                    .unwrap()
                };
                runs.push((backend, result, t.elapsed().as_secs_f64() * 1000.0));
            }
            let legacy = &runs.iter().find(|r| r.0 == Backend::Legacy).unwrap().1;
            for (_, result, _) in &runs {
                assert_eq!(result.minimum, legacy.minimum);
                assert_eq!(result.curve, legacy.curve);
            }
            let timing = |backend| runs.iter().find(|r| r.0 == backend).map(|r| r.2);
            let legacy_ms = timing(Backend::Legacy).unwrap();
            let cpu_ms = timing(Backend::Cpu).unwrap();
            let gpu_ms = timing(Backend::Gpu);
            let gpu_used = runs
                .iter()
                .find(|r| r.0 == Backend::Gpu)
                .is_some_and(|r| r.1.backend == Backend::Gpu);
            eprintln!(
                "deep_scan_replay {} round={round} offsets={} legacy={legacy_ms:.1}ms cpu={cpu_ms:.1}ms gpu={gpu_ms:?} used={gpu_used}",
                path.display(),
                grid.steps()
            );
            report.push(serde_json::json!({"file":path.file_name().unwrap().to_string_lossy(), "round":round,
                "offsets":grid.steps(), "observations":of.len(), "gyro":source.len(), "legacy_ms":legacy_ms, "cpu_ms":cpu_ms,
                "gpu_ms":gpu_ms,"gpu_used":gpu_used,"minimum":legacy.minimum,"curve_points":legacy.curve.len(), "exact":true}));
            expected = runs
                .into_iter()
                .find(|r| r.0 == Backend::Legacy)
                .map(|r| r.1);
        }
        let mut expected = expected.unwrap();
        let Some((coarse, _)) = expected.minimum else {
            continue;
        };
        let refined = (0..200)
            .into_par_iter()
            .map(|i| {
                let offset = coarse + (-2.0 + i as f64 * 0.01);
                (offset, super::super::calculate_cost(offset, &of, &source))
            })
            .reduce_with(|a, b| if a.1 < b.1 { a } else { b })
            .unwrap();
        let radius = dm::post_dense_ms();
        for j in 0..=((radius * 2.0 / 5.0) as usize) {
            let offset = refined.0 - radius + j as f64 * 5.0;
            let cost = super::super::calculate_cost(offset, &of, &source);
            if cost.is_finite() && cost != f64::MAX {
                expected.curve.push((offset, cost));
            }
        }
        expected.curve.push(refined);
        if (refined.0 - grid.initial).abs() < grid.radius * 0.9 {
            curves.push(dm::DeepMatchWindowCurve {
                range_idx: header.range_index,
                t_center_ms: header.center_ms,
                argmin_ms: refined.0,
                cost_min: refined.1,
                n_eff: of.len() as f64,
                curve: expected.curve,
            });
        }
    }
    let verdict = dm::decide_posterior(
        &curves,
        curves.len(),
        0,
        clip_ms,
        dm::post_conf_min(),
        dm::post_ci95_base_ms(),
        dm::drift_rate_ms_per_min(),
        dm::drift_floor_ms(),
    );
    let out = serde_json::json!({"runs":report,"posterior":format!("{verdict:?}"),"exact":true,
        "scope":"frozen coarse scan and joint posterior; complete probe verdict is recorded by the footage harness",
        "threads":rayon::current_num_threads()});
    std::fs::write(
        root.join("replay.json"),
        serde_json::to_vec_pretty(&out).unwrap(),
    )
    .unwrap();
}
