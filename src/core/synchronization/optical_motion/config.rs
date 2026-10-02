// SPDX-License-Identifier: GPL-3.0-or-later

//! Tuning knobs for the optical motion sync method, resolved once from env vars.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug)]
pub struct OpticalConfig {
    pub g_full: f64,
    pub replenish: f64,
    pub coarse_step_ms: f64,
    pub coarse_points: usize,
    pub track_width: u32,
}

/// Returns (value, "env" | "default"); warns and falls back when `raw` does not parse or `valid` rejects it.
pub fn parse_env_f64(name: &str, raw: Option<&str>, default: f64, valid: impl Fn(f64) -> bool) -> (f64, &'static str) {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else { return (default, "default"); };
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && valid(v) => (v, "env"),
        _ => {
            log::warn!(target: "lifecycle", "{}={} invalid, falling back to default ({})", name, s, default);
            (default, "default")
        }
    }
}

fn resolve(suffix: &str, default: f64, valid: impl Fn(f64) -> bool) -> f64 {
    let name = format!("GYROFLOW_SYNC_OPTICAL_{}", suffix);
    let raw = std::env::var(&name).ok();
    let (value, source) = parse_env_f64(&name, raw.as_deref(), default, valid);
    log::info!(target: "lifecycle", "sync_optical_{} resolved value={} source={}", suffix.to_ascii_lowercase(), value, source);
    value
}

pub fn config() -> &'static OpticalConfig {
    static RESOLVED: OnceLock<OpticalConfig> = OnceLock::new();
    RESOLVED.get_or_init(|| OpticalConfig {
        g_full: resolve("G_FULL", 2.0, |v| v > 1.0),
        replenish: resolve("REPLENISH", 0.85, |v| v > 0.0 && v <= 1.0),
        coarse_step_ms: resolve("COARSE_STEP_MS", 10.0, |v| (1.0..=50.0).contains(&v)),
        coarse_points: resolve("COARSE_POINTS", 200.0, |v| v >= 50.0) as usize,
        track_width: resolve("TRACK_WIDTH", 960.0, |v| v >= 320.0) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn parse_env_falls_back_on_invalid() {
        assert_eq!(parse_env_f64("X", None, 2.0, |v| v > 1.0), (2.0, "default"));
        assert_eq!(parse_env_f64("X", Some("3.5"), 2.0, |v| v > 1.0), (3.5, "env"));
        assert_eq!(parse_env_f64("X", Some("0.5"), 2.0, |v| v > 1.0), (2.0, "default"));
        assert_eq!(parse_env_f64("X", Some("abc"), 2.0, |v| v > 1.0), (2.0, "default"));
    }
}
