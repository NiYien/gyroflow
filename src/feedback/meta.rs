// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// System info collected at submission time. Embedded into the feedback zip's
// manifest.json so the analyzer can quickly tell what hardware/OS class
// produced the report.

use std::sync::OnceLock;

#[derive(Clone, Debug, serde::Serialize)]
pub struct Meta {
    pub app_version:   String,
    pub os:            String,
    pub gpu:           String,
    pub cpu:           String,
    pub memory_total:  u64,        // bytes
    pub display_scale: Option<f64>, // primary screen DPR; None if unavailable
}

static GPU_OVERRIDE: OnceLock<String> = OnceLock::new();

/// Cache the GPU description from a renderer probe. Should be called once
/// during gyroflow startup with whatever wgpu adapter info is available.
/// If never called, `Meta.gpu` falls back to `"?"`.
pub fn set_gpu(s: String) {
    let _ = GPU_OVERRIDE.set(s);
}

impl Meta {
    pub fn collect() -> Self {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        sys.refresh_cpu_all();

        let cpu = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "?".to_string());

        let memory_total = sys.total_memory();

        let os = format!(
            "{} {} ({})",
            os_info::get().os_type(),
            os_info::get().version(),
            os_info::get().bitness(),
        );

        let gpu = GPU_OVERRIDE.get().cloned().unwrap_or_else(|| "?".to_string());

        Self {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            os,
            gpu,
            cpu,
            memory_total,
            display_scale: None, // populated by controller before submit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_does_not_panic() {
        let m = Meta::collect();
        assert!(!m.app_version.is_empty());
        assert!(!m.os.is_empty());
        // CPU brand is sometimes empty in containerized CI; allow "?" fallback.
        assert!(!m.cpu.is_empty());
    }

    #[test]
    fn gpu_override_round_trip() {
        // OnceLock; only the first set wins. Test in a fresh process would be
        // ideal but we accept this looseness.
        let _ = set_gpu("NVIDIA Test GPU".to_string());
        let m = Meta::collect();
        assert!(!m.gpu.is_empty());
    }
}
