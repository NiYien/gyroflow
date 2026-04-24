use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracePhase {
    None,
    TensorUpload,
    Forward,
    Readback,
}

#[derive(Debug, Clone, Copy)]
pub struct TraceContext {
    pub seq: Option<u64>,
    pub phase: TracePhase,
}

#[derive(Debug, Clone, Copy)]
pub enum DurationMetric {
    TensorUpload,
    ForwardTotal,
    ForwardClientSubmit,
    ForwardClientOutputAlloc,
    ForwardClientEnqueue,
    ForwardClientBackpressure,
    ForwardRunnerTask,
    ForwardServerRegister,
    ForwardPolicyUpdate,
    ForwardPlanFind,
    ReadbackTotal,
    ReadbackDrain,
    ReadbackFlushSubmit,
    ReadbackCopyToStaging,
    ReadbackMapWait,
    ReadbackGetMappedRange,
    ChannelRoundtrip,
}

#[derive(Debug, Clone, Copy)]
pub enum CounterMetric {
    ForwardPlanHit,
    ForwardPlanMiss,
    ForwardPlanAdd,
    ForwardActionDefer,
    ForwardRegisterCall,
}

#[derive(Debug, Default, Clone)]
pub struct FrameTrace {
    pub tensor_upload_ms: f64,
    pub forward_total_ms: f64,
    pub forward_client_submit_ms: f64,
    pub forward_client_output_alloc_ms: f64,
    pub forward_client_enqueue_ms: f64,
    pub forward_client_backpressure_ms: f64,
    pub forward_runner_task_ms: f64,
    pub forward_server_register_ms: f64,
    pub forward_policy_update_ms: f64,
    pub forward_plan_find_ms: f64,
    pub forward_register_call_count: u64,
    pub forward_plan_hit_count: u64,
    pub forward_plan_miss_count: u64,
    pub forward_plan_add_count: u64,
    pub forward_action_defer_count: u64,
    pub readback_total_ms: f64,
    pub readback_drain_ms: f64,
    pub readback_flush_submit_ms: f64,
    pub readback_gpu_profile_ms: Option<f64>,
    pub readback_copy_to_staging_ms: f64,
    pub readback_map_wait_ms: f64,
    pub readback_get_mapped_range_ms: f64,
    pub channel_roundtrip_ms: f64,
    pub overlap_current_seq: Option<u64>,
}

pub struct TraceContextGuard {
    prev_seq: Option<u64>,
    prev_phase: TracePhase,
}

thread_local! {
    static CURRENT_SEQ: Cell<Option<u64>> = const { Cell::new(None) };
    static CURRENT_PHASE: Cell<TracePhase> = const { Cell::new(TracePhase::None) };
}

static TRACE_LEVEL: OnceLock<u8> = OnceLock::new();
static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);
static FRAME_TRACES: OnceLock<Mutex<HashMap<u64, FrameTrace>>> = OnceLock::new();

fn traces() -> &'static Mutex<HashMap<u64, FrameTrace>> {
    FRAME_TRACES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn with_frame(seq: u64, f: impl FnOnce(&mut FrameTrace)) {
    let mut guard = traces().lock().expect("trace mutex poisoned");
    let frame = guard.entry(seq).or_default();
    f(frame);
}

pub fn trace_level() -> u8 {
    *TRACE_LEVEL.get_or_init(|| {
        std::env::var("NEUFLOW_TRACE")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(0)
    })
}

pub fn enabled() -> bool {
    trace_level() > 0
}

pub fn verbose() -> bool {
    trace_level() >= 2
}

pub fn next_seq() -> u64 {
    NEXT_SEQ.fetch_add(1, Ordering::Relaxed)
}

pub fn capture_context() -> TraceContext {
    TraceContext {
        seq: current_seq(),
        phase: current_phase(),
    }
}

pub fn enter(seq: Option<u64>, phase: TracePhase) -> TraceContextGuard {
    let prev_seq = CURRENT_SEQ.with(|cell| cell.replace(seq));
    let prev_phase = CURRENT_PHASE.with(|cell| cell.replace(phase));

    TraceContextGuard {
        prev_seq,
        prev_phase,
    }
}

pub fn enter_context(ctx: TraceContext) -> TraceContextGuard {
    enter(ctx.seq, ctx.phase)
}

impl Drop for TraceContextGuard {
    fn drop(&mut self) {
        CURRENT_SEQ.with(|cell| cell.set(self.prev_seq));
        CURRENT_PHASE.with(|cell| cell.set(self.prev_phase));
    }
}

pub fn current_seq() -> Option<u64> {
    CURRENT_SEQ.with(Cell::get)
}

pub fn current_phase() -> TracePhase {
    CURRENT_PHASE.with(Cell::get)
}

pub fn record_duration(seq: u64, metric: DurationMetric, duration: Duration) {
    if !enabled() {
        return;
    }
    with_frame(seq, |frame| {
        let ms = duration_ms(duration);
        match metric {
            DurationMetric::TensorUpload => frame.tensor_upload_ms += ms,
            DurationMetric::ForwardTotal => frame.forward_total_ms += ms,
            DurationMetric::ForwardClientSubmit => frame.forward_client_submit_ms += ms,
            DurationMetric::ForwardClientOutputAlloc => frame.forward_client_output_alloc_ms += ms,
            DurationMetric::ForwardClientEnqueue => frame.forward_client_enqueue_ms += ms,
            DurationMetric::ForwardClientBackpressure => frame.forward_client_backpressure_ms += ms,
            DurationMetric::ForwardRunnerTask => frame.forward_runner_task_ms += ms,
            DurationMetric::ForwardServerRegister => frame.forward_server_register_ms += ms,
            DurationMetric::ForwardPolicyUpdate => frame.forward_policy_update_ms += ms,
            DurationMetric::ForwardPlanFind => frame.forward_plan_find_ms += ms,
            DurationMetric::ReadbackTotal => frame.readback_total_ms += ms,
            DurationMetric::ReadbackDrain => frame.readback_drain_ms += ms,
            DurationMetric::ReadbackFlushSubmit => frame.readback_flush_submit_ms += ms,
            DurationMetric::ReadbackCopyToStaging => frame.readback_copy_to_staging_ms += ms,
            DurationMetric::ReadbackMapWait => frame.readback_map_wait_ms += ms,
            DurationMetric::ReadbackGetMappedRange => frame.readback_get_mapped_range_ms += ms,
            DurationMetric::ChannelRoundtrip => frame.channel_roundtrip_ms += ms,
        }
    });
}

pub fn record_duration_current(metric: DurationMetric, duration: Duration) {
    if let Some(seq) = current_seq() {
        record_duration(seq, metric, duration);
    }
}

pub fn record_counter(seq: u64, metric: CounterMetric, delta: u64) {
    if !enabled() {
        return;
    }
    with_frame(seq, |frame| match metric {
        CounterMetric::ForwardPlanHit => frame.forward_plan_hit_count += delta,
        CounterMetric::ForwardPlanMiss => frame.forward_plan_miss_count += delta,
        CounterMetric::ForwardPlanAdd => frame.forward_plan_add_count += delta,
        CounterMetric::ForwardActionDefer => frame.forward_action_defer_count += delta,
        CounterMetric::ForwardRegisterCall => frame.forward_register_call_count += delta,
    });
}

pub fn record_counter_current(metric: CounterMetric, delta: u64) {
    if let Some(seq) = current_seq() {
        record_counter(seq, metric, delta);
    }
}

pub fn set_gpu_profile_ms(seq: u64, value: Option<f64>) {
    if !enabled() {
        return;
    }
    with_frame(seq, |frame| {
        if let Some(value) = value {
            frame.readback_gpu_profile_ms = Some(value);
        }
    });
}

pub fn set_gpu_profile_ms_current(value: Option<f64>) {
    if let Some(seq) = current_seq() {
        set_gpu_profile_ms(seq, value);
    }
}

pub fn note_overlap(owner_seq: u64, current_seq: u64) {
    if !enabled() {
        return;
    }
    with_frame(owner_seq, |frame| {
        frame.overlap_current_seq = Some(current_seq);
    });
}

pub fn take_frame(seq: u64) -> Option<FrameTrace> {
    if !enabled() {
        return None;
    }
    traces().lock().expect("trace mutex poisoned").remove(&seq)
}
