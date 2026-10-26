// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! Dense deep-match scans with the same timestamp lookup and score as essential_matrix.

use crate::gyro_source::TimeIMU;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

#[path = "essential_scan_audit.rs"]
mod audit;
pub(super) use audit::capture;
#[path = "essential_scan_gpu.rs"]
mod gpu;

const BLOCK: usize = 1024;
const MAX_PACKED_BYTES: usize = 384 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Backend {
    Legacy,
    Cpu,
    Gpu,
    Auto,
}

pub(super) fn backend() -> Backend {
    static VALUE: std::sync::OnceLock<Backend> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        let value = match std::env::var("GYROFLOW_DEEP_SCAN_BACKEND").as_deref() {
            Ok("legacy") => Backend::Legacy,
            Ok("cpu") | Err(_) => Backend::Cpu,
            Ok("gpu") => Backend::Gpu,
            Ok("auto") => Backend::Auto,
            Ok(other) => {
                log::warn!(target: "sync", "Unknown deep scan backend {other}, using cpu");
                Backend::Cpu
            }
        };
        log::info!(target: "lifecycle", "deep_scan_backend resolved={value:?}");
        value
    })
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Grid {
    pub initial: f64,
    pub radius: f64,
}

impl Grid {
    pub fn steps(self) -> usize {
        self.radius as usize * 2
    }
    pub fn curve_steps(self) -> usize {
        (self.radius * 2.0 / 25.0) as usize
    }
    pub fn offset(self, index: usize) -> f64 {
        self.initial - self.radius + index as f64
    }
    pub fn valid(self) -> bool {
        self.initial.is_finite()
            && self.radius.is_finite()
            && self.radius >= 0.0
            && self.radius < (usize::MAX / 2) as f64
    }
}

pub(super) struct PackedGyro {
    pub times: Vec<usize>,
    pub rates: Vec<[f64; 3]>,
    pub present: Vec<bool>,
}

impl PackedGyro {
    pub fn new(source: &BTreeMap<usize, TimeIMU>) -> Option<Self> {
        if source.len().checked_mul(33)? > MAX_PACKED_BYTES {
            return None;
        }
        let mut out = Self {
            times: Vec::new(),
            rates: Vec::new(),
            present: Vec::new(),
        };
        out.times.try_reserve_exact(source.len()).ok()?;
        out.rates.try_reserve_exact(source.len()).ok()?;
        out.present.try_reserve_exact(source.len()).ok()?;
        for (&time, sample) in source {
            out.times.push(time);
            out.rates.push(sample.gyro.unwrap_or_default());
            out.present.push(sample.gyro.is_some());
        }
        Some(out)
    }

    /// The original map's range(query..).next(), including Rust's saturating float cast.
    fn index(&self, time_ms: f64) -> usize {
        let query = (time_ms * 1000.0) as usize;
        self.times.partition_point(|&time| time < query)
    }

    /// A coarse time index narrows the lookup without changing which sample is selected.
    pub fn indexed_cost(
        &self,
        offset: f64,
        observations: &[TimeIMU],
        origin_us: usize,
        buckets: &[u32],
    ) -> f64 {
        let mut sum = 0.0;
        let mut count = 0usize;
        for sample in observations {
            if let Some(o) = sample.gyro {
                let query = ((sample.timestamp_ms - offset) * 1000.0) as usize;
                let bucket = (query.saturating_sub(origin_us) / 4000).min(buckets.len() - 1);
                let mut index = buckets[bucket] as usize;
                while index < self.times.len() && self.times[index] < query {
                    index += 1;
                }
                if index < self.times.len() && self.present[index] {
                    add_score(&mut sum, self.rates[index], o);
                    count += 1;
                }
            }
        }
        finish_score(sum, count, observations.len())
    }

    /// Each observation visits decreasing query times. Its cursor only moves backwards.
    /// Looping over observations first retains the original addition order for every offset.
    pub fn block(
        &self,
        observations: &[TimeIMU],
        grid: Grid,
        start: usize,
        len: usize,
        stride: usize,
    ) -> Vec<f64> {
        let offsets: Vec<f64> = (0..len).map(|i| grid.offset(start + i * stride)).collect();
        let mut sums = vec![0.0; len];
        let mut counts = vec![0usize; len];
        if len == 0 {
            return sums;
        }
        for sample in observations {
            let Some(o) = sample.gyro else { continue };
            let mut index = self.index(sample.timestamp_ms - offsets[0]);
            for k in 0..len {
                let query = ((sample.timestamp_ms - offsets[k]) * 1000.0) as usize;
                while index > 0 && self.times[index - 1] >= query {
                    index -= 1;
                }
                if index < self.times.len() && self.present[index] {
                    add_score(&mut sums[k], self.rates[index], o);
                    counts[k] += 1;
                }
            }
        }
        for (sum, count) in sums.iter_mut().zip(counts) {
            *sum = finish_score(*sum, count, observations.len());
        }
        sums
    }
}

#[inline]
fn add_score(sum: &mut f64, g: [f64; 3], o: [f64; 3]) {
    *sum += (g[0] - o[0]).powi(2) * 70.0;
    *sum += (g[1] - o[1]).powi(2) * 70.0;
    *sum += (g[2] - o[2]).powi(2) * 100.0;
}

#[inline]
fn finish_score(sum: f64, count: usize, observations: usize) -> f64 {
    if observations > 0 && count > observations / 2 {
        sum / count as f64
    } else {
        f64::MAX
    }
}

#[derive(Debug)]
pub(super) struct ScanResult {
    pub minimum: Option<(f64, f64)>,
    pub curve: Vec<(f64, f64)>,
    pub backend: Backend,
}

/// Rightmost minimum, matching the ordered parallel reduction in the original scanner.
fn choose(a: (usize, f64), b: (usize, f64)) -> (usize, f64) {
    if a.1 < b.1 || (a.1 == b.1 && a.0 > b.0) {
        a
    } else {
        b
    }
}

pub(super) fn cpu_scan(
    observations: &[TimeIMU],
    gyro: &PackedGyro,
    grid: Grid,
    keep_curve: bool,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f64) + Sync),
) -> Option<ScanResult> {
    if !grid.valid() || cancel.load(Relaxed) {
        return None;
    }
    let steps = grid.steps();
    let done = AtomicUsize::new(0);
    let bucket = AtomicUsize::new(0);
    let blocks: Option<Vec<_>> = (0..steps.div_ceil(BLOCK))
        .into_par_iter()
        .map(|block| {
            if cancel.load(Relaxed) {
                return None;
            }
            let start = block * BLOCK;
            let len = BLOCK.min(steps - start);
            let scores = gyro.block(observations, grid, start, len, 1);
            let minimum = scores
                .iter()
                .enumerate()
                .map(|(i, &c)| (start + i, c))
                .reduce(choose);
            let curve: Vec<_> = if keep_curve {
                scores
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, cost)| {
                        let j = start + i;
                        (j % 25 == 0
                            && j / 25 < grid.curve_steps()
                            && cost.is_finite()
                            && cost != f64::MAX)
                            .then(|| (grid.offset(j), cost))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let completed = done.fetch_add(len, Relaxed) + len;
            let next = completed * 100 / steps.max(1);
            if bucket.fetch_max(next, Relaxed) < next {
                progress(completed as f64 / steps.max(1) as f64);
            }
            Some((minimum, curve))
        })
        .collect();
    if cancel.load(Relaxed) {
        return None;
    }
    let blocks = blocks?;
    let minimum = blocks
        .iter()
        .filter_map(|b| b.0)
        .reduce(choose)
        .map(|(j, c)| (grid.offset(j), c));
    let curve = blocks.into_iter().flat_map(|b| b.1).collect();
    Some(ScanResult {
        minimum,
        curve,
        backend: Backend::Cpu,
    })
}

pub(super) fn scan(
    observations: &[TimeIMU],
    source: &BTreeMap<usize, TimeIMU>,
    grid: Grid,
    keep_curve: bool,
    device: i32,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f64) + Sync),
) -> Option<ScanResult> {
    scan_with_backend(
        backend(),
        observations,
        source,
        grid,
        keep_curve,
        device,
        cancel,
        progress,
    )
}

fn scan_with_backend(
    selected: Backend,
    observations: &[TimeIMU],
    source: &BTreeMap<usize, TimeIMU>,
    grid: Grid,
    keep_curve: bool,
    device: i32,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f64) + Sync),
) -> Option<ScanResult> {
    if selected == Backend::Legacy || !grid.valid() {
        return None;
    }
    let started = std::time::Instant::now();
    let packed = PackedGyro::new(source)?;
    // NaN reductions are not associative; retain the original reduction for such data.
    if packed.rates.iter().flatten().any(|v| !v.is_finite())
        || observations
            .iter()
            .filter_map(|s| s.gyro)
            .flatten()
            .any(|v| !v.is_finite())
    {
        return None;
    }
    let prepared_ms = started.elapsed().as_secs_f64() * 1000.0;
    let latest = std::sync::Mutex::new(0.0f64);
    let monotonic = |p: f64| {
        let mut previous = latest.lock().unwrap_or_else(|e| e.into_inner());
        let p = p.clamp(0.0, 1.0);
        if p > *previous {
            *previous = p;
            progress(p);
        }
    };
    let use_gpu = usize::BITS == 64 && device >= 0
        && (selected == Backend::Gpu
            || (selected == Backend::Auto
                && grid.steps() >= 1_000_000
            // The measured 211/250-observation two-hour scans favor CPU on cold runs.
            // Reserve automatic GPU use for substantially larger work; explicit GPU remains available.
            && grid.steps().saturating_mul(observations.len()) >= 3_000_000_000));
    if use_gpu {
        match gpu::gpu_scan(
            observations,
            &packed,
            grid,
            keep_curve,
            device,
            cancel,
            &monotonic,
        ) {
            Ok(result) => {
                log::info!(target: "sync", "[deep-scan] backend=gpu offsets={} prep_ms={prepared_ms:.1} total_ms={:.1}",
                    grid.steps(), started.elapsed().as_secs_f64() * 1000.0);
                return Some(result);
            }
            Err(_) if cancel.load(Relaxed) => return None,
            Err(error) => log::info!(target: "sync", "[deep-scan] GPU fallback to CPU: {error}"),
        }
    }
    let result = cpu_scan(observations, &packed, grid, keep_curve, cancel, &monotonic);
    log::info!(target: "sync", "[deep-scan] backend={:?} offsets={} samples={} gyro={} prep_ms={prepared_ms:.1} total_ms={:.1}",
        result.as_ref().map_or(Backend::Cpu, |r| r.backend), grid.steps(), observations.len(), source.len(), started.elapsed().as_secs_f64() * 1000.0);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(time: f64, rates: Option<[f64; 3]>) -> TimeIMU {
        TimeIMU {
            timestamp_ms: time,
            gyro: rates,
            ..Default::default()
        }
    }

    fn fixture() -> (Vec<TimeIMU>, BTreeMap<usize, TimeIMU>) {
        let gyro = (0..12000)
            .map(|i| {
                let time = i * 777 + i % 7;
                let x = i as f64 * 0.017;
                (
                    time,
                    sample(
                        time as f64 / 1000.0,
                        (i % 19 != 0).then_some([x.sin(), x.cos(), (x * 0.7).sin()]),
                    ),
                )
            })
            .collect();
        let of = (0..71)
            .map(|i| {
                let x = i as f64 * 0.131;
                sample(
                    1000.000001 + i as f64 * 16.683333,
                    (i % 13 != 0).then_some([x.sin(), x.cos(), x]),
                )
            })
            .collect();
        (of, gyro)
    }

    #[test]
    fn packed_deep_scan_matches_every_original_score() {
        let (of, source) = fixture();
        let gyro = PackedGyro::new(&source).unwrap();
        for grid in [
            Grid {
                initial: -1000.1234567,
                radius: 4500.9876,
            },
            Grid {
                initial: 0.00000001,
                radius: 999.51,
            },
        ] {
            for start in (0..grid.steps()).step_by(93) {
                let scores = gyro.block(&of, grid, start, 93.min(grid.steps() - start), 1);
                for (j, cost) in scores.iter().enumerate() {
                    let expected =
                        super::super::calculate_cost(grid.offset(start + j), &of, &source);
                    assert_eq!(
                        cost.to_bits(),
                        expected.to_bits(),
                        "offset {}",
                        grid.offset(start + j)
                    );
                }
            }
            let result =
                cpu_scan(&of, &gyro, grid, true, &AtomicBool::new(false), &|_| {}).unwrap();
            let old = (0..grid.steps())
                .into_par_iter()
                .map(|j| {
                    (
                        grid.offset(j),
                        super::super::calculate_cost(grid.offset(j), &of, &source),
                    )
                })
                .reduce_with(|a, b| if a.1 < b.1 { a } else { b });
            assert_eq!(result.minimum, old);
            let expected: Vec<_> = (0..grid.curve_steps())
                .map(|j| {
                    let offset = grid.initial - grid.radius + j as f64 * 25.0;
                    (offset, super::super::calculate_cost(offset, &of, &source))
                })
                .filter(|(_, c)| c.is_finite() && *c != f64::MAX)
                .collect();
            assert_eq!(result.curve, expected);
        }
    }

    #[test]
    fn packed_deep_scan_preserves_missing_boundary_and_tied_scores() {
        for observations in [
            vec![],
            vec![sample(-5.0, Some([1.0; 3]))],
            vec![
                sample(f64::NAN, Some([1.0; 3])),
                sample(f64::INFINITY, None),
            ],
            vec![sample(1000.00000001, Some([0.0; 3])); 30],
        ] {
            let source = BTreeMap::from([
                (1000000, sample(1000.0, Some([0.0; 3]))),
                (1001000, sample(1001.0, None)),
            ]);
            let gyro = PackedGyro::new(&source).unwrap();
            let grid = Grid {
                initial: 0.0,
                radius: 2500.75,
            };
            let scores = gyro.block(&observations, grid, 0, grid.steps(), 1);
            for (j, c) in scores.iter().enumerate() {
                assert_eq!(
                    c.to_bits(),
                    super::super::calculate_cost(grid.offset(j), &observations, &source).to_bits()
                );
            }
            let result = cpu_scan(
                &observations,
                &gyro,
                grid,
                false,
                &AtomicBool::new(false),
                &|_| {},
            )
            .unwrap();
            let old = (0..grid.steps())
                .into_par_iter()
                .map(|j| {
                    (
                        grid.offset(j),
                        super::super::calculate_cost(grid.offset(j), &observations, &source),
                    )
                })
                .reduce_with(|a, b| if a.1 < b.1 { a } else { b });
            assert_eq!(result.minimum, old);
        }
    }

    #[test]
    fn packed_deep_scan_cancellation_returns_no_result() {
        let (of, source) = fixture();
        let gyro = PackedGyro::new(&source).unwrap();
        let cancel = AtomicBool::new(false);
        let grid = Grid {
            initial: 0.0,
            radius: 100000.0,
        };
        assert!(
            cpu_scan(&of, &gyro, grid, true, &cancel, &|_| {
                cancel.store(true, Relaxed);
            })
            .is_none()
        );
        assert!(
            cpu_scan(&of, &gyro, grid, true, &cancel, &|_| panic!(
                "already cancelled"
            ))
            .is_none()
        );
    }

    #[test]
    fn packed_deep_scan_cpu_selection_and_legacy_fallback() {
        let (of, mut source) = fixture();
        let grid = Grid {
            initial: -100.0,
            radius: 1000.5,
        };
        let cancel = AtomicBool::new(false);
        let expected = cpu_scan(
            &of,
            &PackedGyro::new(&source).unwrap(),
            grid,
            true,
            &cancel,
            &|_| {},
        )
        .unwrap();
        let cpu = scan_with_backend(Backend::Gpu, &of, &source, grid, true, -1, &cancel, &|_| {})
            .unwrap();
        assert_eq!(cpu.minimum, expected.minimum);
        assert_eq!(cpu.curve, expected.curve);
        assert!(
            scan_with_backend(
                Backend::Legacy,
                &of,
                &source,
                grid,
                true,
                0,
                &cancel,
                &|_| {}
            )
            .is_none()
        );
        source.values_mut().next().unwrap().gyro = Some([f64::NAN; 3]);
        assert!(
            scan_with_backend(Backend::Cpu, &of, &source, grid, true, 0, &cancel, &|_| {})
                .is_none()
        );
    }
}
