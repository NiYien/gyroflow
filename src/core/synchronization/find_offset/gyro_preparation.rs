// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! Reuse an identical filtered gyro slice within one immutable deep-probe snapshot.

use crate::{filtering::Lowpass, gyro_source::TimeIMU};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let value = std::env::var("GYROFLOW_DEEP_PREPARE_REUSE")
            .map(|s| {
                !matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "0" | "off" | "false" | "no"
                )
            })
            .unwrap_or(true);
        log::info!(target: "lifecycle", "deep_prepare_reuse resolved={value}");
        value
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    start: usize,
    end: usize,
    rate: u64,
}

pub(super) struct PreparedGyro<'a> {
    raw: &'a [TimeIMU],
    ordered: bool,
    cached: Option<(Key, Arc<BTreeMap<usize, TimeIMU>>)>,
}

impl<'a> PreparedGyro<'a> {
    pub fn new(raw: &'a [TimeIMU]) -> Self {
        let ordered = raw.first().is_none_or(|s| s.timestamp_ms.is_finite())
            && raw
                .windows(2)
                .all(|s| s[1].timestamp_ms.is_finite() && s[0].timestamp_ms <= s[1].timestamp_ms);
        Self {
            raw,
            ordered,
            cached: None,
        }
    }

    pub fn window(
        &mut self,
        from: f64,
        to: f64,
        initial: f64,
        radius: f64,
        rate: f64,
    ) -> (Arc<BTreeMap<usize, TimeIMU>>, bool) {
        let lower = from - radius;
        let upper = to + radius;
        let key = if self.ordered && [lower, upper, initial].iter().all(|x| x.is_finite()) {
            // Keep the original addition and comparisons; do not algebraically shift the bounds.
            let start = self
                .raw
                .partition_point(|s| s.timestamp_ms + initial < lower);
            let end = self
                .raw
                .partition_point(|s| s.timestamp_ms + initial <= upper);
            Some(Key {
                start: start.min(end),
                end,
                rate: rate.to_bits(),
            })
        } else {
            None
        };
        if let Some(key) = key {
            if let Some((old_key, table)) = &self.cached {
                if *old_key == key {
                    return (table.clone(), true);
                }
            }
        }
        // Release the old table before constructing a different large slice.
        self.cached = None;
        let mut selected: Vec<TimeIMU> = match key {
            Some(key) => self.raw[key.start..key.end].to_vec(),
            None => self
                .raw
                .iter()
                .filter(|s| {
                    let ts = s.timestamp_ms + initial;
                    ts >= lower && ts <= upper
                })
                .cloned()
                .collect(),
        };
        let _ = Lowpass::filter_gyro_forward_backward(20.0, rate, &mut selected);
        let table = Arc::new(
            selected
                .into_iter()
                .map(|s| ((s.timestamp_ms * 1000.0) as usize, s))
                .collect(),
        );
        if let Some(key) = key {
            self.cached = Some((key, Arc::clone(&table)));
        }
        (table, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(
        raw: &[TimeIMU],
        from: f64,
        to: f64,
        initial: f64,
        radius: f64,
        rate: f64,
    ) -> BTreeMap<usize, TimeIMU> {
        let mut selected: Vec<_> = raw
            .iter()
            .filter(|s| {
                let ts = s.timestamp_ms + initial;
                ts >= from - radius && ts <= to + radius
            })
            .cloned()
            .collect();
        let _ = Lowpass::filter_gyro_forward_backward(20.0, rate, &mut selected);
        selected
            .into_iter()
            .map(|s| ((s.timestamp_ms * 1000.0) as usize, s))
            .collect()
    }

    fn assert_same(a: &BTreeMap<usize, TimeIMU>, b: &BTreeMap<usize, TimeIMU>) {
        assert_eq!(a.len(), b.len());
        for ((ka, a), (kb, b)) in a.iter().zip(b) {
            assert_eq!(ka, kb);
            assert_eq!(a.timestamp_ms.to_bits(), b.timestamp_ms.to_bits());
            assert_eq!(
                a.gyro.map(|v| v.map(f64::to_bits)),
                b.gyro.map(|v| v.map(f64::to_bits))
            );
            assert_eq!(
                a.accl.map(|v| v.map(f64::to_bits)),
                b.accl.map(|v| v.map(f64::to_bits))
            );
            assert_eq!(
                a.magn.map(|v| v.map(f64::to_bits)),
                b.magn.map(|v| v.map(f64::to_bits))
            );
        }
    }

    fn samples() -> Vec<TimeIMU> {
        (0..3000)
            .map(|i| TimeIMU {
                timestamp_ms: i as f64 * 0.997 + 0.0000001,
                gyro: (i % 13 != 0).then_some([(i as f64 * 0.03).sin(), 2.0, -3.0]),
                accl: Some([1.0, 2.0, 3.0]),
                magn: None,
            })
            .collect()
    }

    #[test]
    fn deep_prepare_matches_original_filter_and_map() {
        let mut raw = samples();
        raw[501].timestamp_ms = raw[500].timestamp_ms;
        let mut cache = PreparedGyro::new(&raw);
        for (from, to, initial, radius, rate) in [
            (0.0, 100.0, -9.123456, 4000.0, 1000.0),
            (80.1, 110.7, -9.123456, 4000.0, 1000.0),
            (100.0000001, 1999.0000001, 0.00000001, 0.0, 1000.0),
            (100.0000001, 1999.0000001, 0.00000001, 0.0, 10.0),
            (300.0, 100.0, 0.0, 0.0, 1000.0),
        ] {
            let (table, _) = cache.window(from, to, initial, radius, rate);
            assert_same(&table, &reference(&raw, from, to, initial, radius, rate));
        }
    }

    #[test]
    fn deep_prepare_reuses_only_identical_ranges_and_rates() {
        let raw = samples();
        let mut cache = PreparedGyro::new(&raw);
        let (a, hit) = cache.window(0.0, 100.0, 0.0, 4000.0, 1000.0);
        assert!(!hit);
        let (b, hit) = cache.window(200.0, 400.0, 0.0, 4000.0, 1000.0);
        assert!(hit && Arc::ptr_eq(&a, &b));
        assert!(!cache.window(0.0, 100.0, 0.0, 4000.0, 999.0).1);
        assert!(!cache.window(0.0, 100.0, 0.0, 0.0, 999.0).1);
        assert!(
            !PreparedGyro::new(&raw)
                .window(0.0, 100.0, 0.0, 4000.0, 1000.0)
                .1
        );
    }

    #[test]
    fn deep_prepare_keeps_unordered_and_nonfinite_selection_semantics() {
        let mut raw = samples();
        raw.swap(10, 1000);
        raw[30].timestamp_ms = f64::NAN;
        let mut cache = PreparedGyro::new(&raw);
        for _ in 0..2 {
            let (table, hit) = cache.window(100.0, 1700.0, -50.0, 20.0, 1000.0);
            assert!(!hit);
            assert_same(&table, &reference(&raw, 100.0, 1700.0, -50.0, 20.0, 1000.0));
        }
    }
}
