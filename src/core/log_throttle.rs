// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 gyroflow-niyien

// Token-bucket throttle for per-frame log records. Each unique key tracks the
// last successful emit time; `try_emit` returns true at most once per
// `min_interval_ms`, the rest are suppressed. Used by stab.timing per-backend
// telemetry so 60 fps × N backends does not flood the file. Keys are pairs of
// `&'static str` so the map lookup avoids allocation on the hot path.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;
use parking_lot::Mutex;

type ThrottleMap = Mutex<HashMap<(&'static str, &'static str), Instant>>;

fn throttle_map() -> &'static ThrottleMap {
    static MAP: OnceLock<ThrottleMap> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns true when at least `min_interval_ms` has elapsed since the last
/// `true` for this key (or this is the first call for the key). On `true`
/// the key's last-emit timestamp is updated to now.
///
/// Thread-safe; the mutex is held only across the map lookup + timestamp
/// compare, so contention is sub-microsecond even under sustained calls.
pub fn try_emit(key: (&'static str, &'static str), min_interval_ms: u64) -> bool {
    let now = Instant::now();
    let mut map = throttle_map().lock();
    match map.get(&key) {
        Some(last) if now.duration_since(*last).as_millis() < min_interval_ms as u128 => false,
        _ => {
            map.insert(key, now);
            true
        }
    }
}

/// Reads `GYROFLOW_STAB_TIMING_MS` once per process and caches the parsed value.
/// Falls back to `default_ms` on parse failure or missing env var. Subsequent
/// calls return the cached value without consulting the environment.
pub fn min_interval_ms_from_env(default_ms: u64) -> u64 {
    static CACHED: OnceLock<u64> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("GYROFLOW_STAB_TIMING_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(default_ms)
    })
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    throttle_map().lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::thread;
    use std::time::Duration;

    #[test]
    #[serial]
    fn first_call_returns_true_and_second_immediate_call_returns_false() {
        reset_for_test();
        let key = ("a", "b");
        assert!(try_emit(key, 1000));
        assert!(!try_emit(key, 1000));
    }

    #[test]
    #[serial]
    fn second_call_after_min_interval_returns_true_again() {
        reset_for_test();
        let key = ("c", "d");
        assert!(try_emit(key, 50));
        thread::sleep(Duration::from_millis(80));
        assert!(try_emit(key, 50));
    }

    #[test]
    #[serial]
    fn different_keys_do_not_interfere() {
        reset_for_test();
        assert!(try_emit(("k1", "x"), 1000));
        assert!(try_emit(("k1", "y"), 1000));
        assert!(try_emit(("k2", "x"), 1000));
        assert!(!try_emit(("k1", "x"), 1000));
    }

    #[test]
    fn env_interval_cached_after_first_read() {
        // OnceLock semantics: the first read wins. We can only observe the
        // cache by asserting two reads return the same value (cached path).
        // No #[serial] needed — env cache is a one-shot OnceLock, observable
        // independent of the throttle map state.
        let a = min_interval_ms_from_env(123);
        let b = min_interval_ms_from_env(456);
        assert_eq!(a, b);
    }
}
