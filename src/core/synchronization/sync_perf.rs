// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! 同步链路的轻量聚合 timing 统计。
//!
//! 每个 [`Stage`] 变体对应一个静态 `StageStats`，通过 [`StageGuard`]
//! RAII 在 drop 时原子累加耗时。`finished_feeding_frames` 入口 [`reset`]、
//! 末尾 [`dump_and_reset`] 打印 ASCII 表格并清零。
//!
//! 热路径开销：3 次 relaxed atomic fetch_add + 1 次 CAS 循环（max 更新），
//! 相对 feed_frame 毫秒级耗时可忽略（<100ns）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Copy, Clone, Debug)]
#[repr(usize)]
pub enum Stage {
    FeedFrame = 0,
    YuvToGray = 1,
    Nv12Clone = 2,
    TaskQueueLatency = 3,
    DetectFeatures = 4,
    ProcessDetected = 5,
    PairOpticalFlow = 6,
    PreprocessNv12 = 7,
    ComputeGridPoints = 8,
    InferAndSample = 9,
    EstimatePose = 10,
    SpinWait = 11,
    CacheOpticalFlow = 12,
    Cleanup = 13,
    FindOffPrep = 14,
    FindOffCoarse = 15,
    FindOffRefine = 16,
    RsSyncFullSync = 17,
    RsSyncGuessOrient = 18,
    DecodeNv12Concat = 19,
    RecalculateGyro = 20,
    FindOffsetsTotal = 21,
    NccFusionDecide = 22,
    NccTikhonov = 23,
    NccCostScan = 24,
    NccFftAlign = 25,
    NccPearsonScan = 26,
    NccOutputPreSync = 27,
    CorrelationRerank = 28,
    RsSyncFinderNew = 29,
    RsSyncCoreFullSync = 30,
}
const NUM_STAGES: usize = 31;

const STAGE_NAMES: [&str; NUM_STAGES] = [
    "feed_frame",
    "  yuv_to_gray",
    "  nv12_clone",
    "task_queue_latency",
    "detect_features",
    "process_detected",
    "  pair_optical_flow",
    "    preprocess_nv12",
    "    compute_grid_points",
    "    infer_and_sample",
    "  estimate_pose",
    "spin_wait",
    "cache_optical_flow",
    "cleanup",
    "findoff.prep",
    "findoff.coarse",
    "findoff.refine",
    "rssync.full_sync",
    "rssync.guess_orient",
    "decode.nv12_concat",
    "recalculate_gyro",
    "find_offsets.total",
    "  ncc_fusion.decide",
    "    ncc_fusion.tikhonov",
    "    ncc_fusion.cost_scan",
    "    ncc_fusion.fft_align",
    "    ncc_fusion.pearson_scan",
    "    ncc_fusion.output_pre_sync",
    "  correlation_rerank",
    "  rssync.finder_new",
    "  rssync.core_full_sync",
];

struct StageStats {
    count: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
}

impl StageStats {
    const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
        }
    }
}

static STATS: [StageStats; NUM_STAGES] = [
    StageStats::new(), StageStats::new(), StageStats::new(), StageStats::new(),
    StageStats::new(), StageStats::new(), StageStats::new(), StageStats::new(),
    StageStats::new(), StageStats::new(), StageStats::new(), StageStats::new(),
    StageStats::new(), StageStats::new(), StageStats::new(), StageStats::new(),
    StageStats::new(), StageStats::new(), StageStats::new(), StageStats::new(),
    StageStats::new(), StageStats::new(), StageStats::new(), StageStats::new(),
    StageStats::new(), StageStats::new(), StageStats::new(), StageStats::new(),
    StageStats::new(), StageStats::new(), StageStats::new(),
];

pub struct StageGuard {
    stage: Stage,
    start: Instant,
}

impl StageGuard {
    pub fn new(stage: Stage) -> Self {
        Self { stage, start: Instant::now() }
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        let ns = self.start.elapsed().as_nanos() as u64;
        record_ns(self.stage, ns);
    }
}

/// 外部直接记录一次耗时（用于非 RAII 场景，例如跨线程测量排队延迟）。
pub fn record_ns(stage: Stage, ns: u64) {
    let s = &STATS[stage as usize];
    s.count.fetch_add(1, Ordering::Relaxed);
    s.total_ns.fetch_add(ns, Ordering::Relaxed);
    let mut prev = s.max_ns.load(Ordering::Relaxed);
    while ns > prev {
        match s.max_ns.compare_exchange_weak(prev, ns, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(p) => prev = p,
        }
    }
}

/// 清零所有 stage 计数。sync 开始时调用。
pub fn reset() {
    for s in STATS.iter() {
        s.count.store(0, Ordering::Relaxed);
        s.total_ns.store(0, Ordering::Relaxed);
        s.max_ns.store(0, Ordering::Relaxed);
    }
}

/// 打印统计表格到 log::info，然后清零。
pub fn dump_and_reset() {
    let mut lines = Vec::with_capacity(NUM_STAGES + 4);
    lines.push("╔════════════════════════════╦═══════╦═════════╦═════════╦═════════╗".to_string());
    lines.push("║ Stage                      ║ Count ║   Total ║     Avg ║     Max ║".to_string());
    lines.push("╠════════════════════════════╬═══════╬═════════╬═════════╬═════════╣".to_string());
    for (i, name) in STAGE_NAMES.iter().enumerate() {
        let s = &STATS[i];
        let count = s.count.load(Ordering::Relaxed);
        if count == 0 {
            continue;
        }
        let total_ns = s.total_ns.load(Ordering::Relaxed);
        let max_ns = s.max_ns.load(Ordering::Relaxed);
        let total_ms = total_ns as f64 / 1_000_000.0;
        let avg_ms = (total_ns as f64 / count as f64) / 1_000_000.0;
        let max_ms = max_ns as f64 / 1_000_000.0;
        lines.push(format!(
            "║ {:<26} ║ {:>5} ║ {:>6.1}ms ║ {:>6.2}ms ║ {:>6.2}ms ║",
            name, count, total_ms, avg_ms, max_ms
        ));
    }
    lines.push("╚════════════════════════════╩═══════╩═════════╩═════════╩═════════╝".to_string());
    let out = lines.join("\n");
    log::info!("[SyncPerf]\n{out}");
    reset();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_accumulates_on_drop() {
        reset();
        {
            let _g = StageGuard::new(Stage::FeedFrame);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let s = &STATS[Stage::FeedFrame as usize];
        assert_eq!(s.count.load(Ordering::Relaxed), 1);
        let total = s.total_ns.load(Ordering::Relaxed);
        assert!(total >= 2_000_000, "expected ≥2ms, got {total}ns");
        assert!(total < 50_000_000, "unreasonably large: {total}ns");
        let max = s.max_ns.load(Ordering::Relaxed);
        assert_eq!(max, total);
    }

    #[test]
    fn record_ns_aggregates() {
        reset();
        record_ns(Stage::InferAndSample, 5_000_000);
        record_ns(Stage::InferAndSample, 10_000_000);
        record_ns(Stage::InferAndSample, 3_000_000);
        let s = &STATS[Stage::InferAndSample as usize];
        assert_eq!(s.count.load(Ordering::Relaxed), 3);
        assert_eq!(s.total_ns.load(Ordering::Relaxed), 18_000_000);
        assert_eq!(s.max_ns.load(Ordering::Relaxed), 10_000_000);
    }

    #[test]
    fn reset_clears_all() {
        record_ns(Stage::EstimatePose, 1000);
        reset();
        for s in STATS.iter() {
            assert_eq!(s.count.load(Ordering::Relaxed), 0);
            assert_eq!(s.total_ns.load(Ordering::Relaxed), 0);
            assert_eq!(s.max_ns.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn dump_and_reset_handles_mixed_zero_nonzero() {
        reset();
        // Stage::FeedFrame has data, others are zero — ensures the skip-zero branch
        // is exercised alongside the non-zero render branch in the same call.
        record_ns(Stage::FeedFrame, 5_000_000);
        dump_and_reset();
        // After dump, all stages must be cleared.
        for s in STATS.iter() {
            assert_eq!(s.count.load(Ordering::Relaxed), 0);
            assert_eq!(s.total_ns.load(Ordering::Relaxed), 0);
            assert_eq!(s.max_ns.load(Ordering::Relaxed), 0);
        }
    }
}
