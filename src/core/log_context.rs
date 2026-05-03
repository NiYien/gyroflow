// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Thread-local logging context: session_id is process-wide, video_path/op are
// per-thread RAII-scoped. Formatter (in `util.rs`) reads this and appends
// `[sid=... vid=... op=...]` to Warn+ lines only.

use std::cell::RefCell;

#[derive(Default, Clone, Debug)]
pub struct LogContext {
    pub session_id: String,
    pub video_path: Option<String>,
    pub op:         Option<String>,
}

#[derive(Default, Clone, Debug)]
pub struct LogContextUpdate {
    pub video_path: Option<Option<String>>,
    pub op:         Option<Option<String>>,
}

impl LogContextUpdate {
    pub fn video_path<S: Into<String>>(mut self, v: S) -> Self { self.video_path = Some(Some(v.into())); self }
    pub fn clear_video_path(mut self) -> Self { self.video_path = Some(None); self }
    pub fn op<S: Into<String>>(mut self, v: S) -> Self { self.op = Some(Some(v.into())); self }
    pub fn clear_op(mut self) -> Self { self.op = Some(None); self }
}

thread_local! {
    static LOG_CTX: RefCell<LogContext> = RefCell::new(LogContext::default());
}

pub struct CtxScope {
    prev: LogContext,
}

impl Drop for CtxScope {
    fn drop(&mut self) {
        let restored = std::mem::take(&mut self.prev);
        LOG_CTX.with(|c| *c.borrow_mut() = restored);
    }
}

impl LogContext {
    /// Set the process-wide session_id. Should be called once during init.
    pub fn set_session_id(sid: String) {
        LOG_CTX.with(|c| c.borrow_mut().session_id = sid);
    }

    /// Apply updates to the current thread's context, returning a guard that
    /// restores the previous context when dropped.
    pub fn enter(updates: LogContextUpdate) -> CtxScope {
        let prev = LOG_CTX.with(|c| c.borrow().clone());
        LOG_CTX.with(|c| {
            let mut current = c.borrow_mut();
            if let Some(v) = updates.video_path { current.video_path = v; }
            if let Some(v) = updates.op { current.op = v; }
        });
        CtxScope { prev }
    }

    /// Read the current thread's context.
    pub fn snapshot() -> LogContext {
        LOG_CTX.with(|c| c.borrow().clone())
    }
}

/// Run a closure with a snapshot of the calling thread's context as the active
/// context. Used to propagate context into worker threads (rayon, std::thread).
pub fn with_ctx<T, F: FnOnce() -> T>(ctx: LogContext, f: F) -> T {
    let prev = LOG_CTX.with(|c| std::mem::replace(&mut *c.borrow_mut(), ctx));
    let result = f();
    LOG_CTX.with(|c| *c.borrow_mut() = prev);
    result
}

/// Snapshot current TLS context and run `f` with it as a fresh context. Useful
/// when handing work to a worker thread that lacks rayon's per-job hooks.
pub fn with_ctx_snapshot<T, F: FnOnce() -> T>(f: F) -> T {
    let snap = LogContext::snapshot();
    with_ctx(snap, f)
}

/// Rayon parallel-iter wrapper: snapshots the current thread's context and
/// re-applies it inside each worker's invocation of `f`. Use at the 5-10
/// known parallel entry points (find_offsets, render_queue par_iter, etc.).
pub fn par_with_ctx<I, F, T>(iter: I, f: F) -> Vec<T>
where
    I: rayon::iter::IntoParallelIterator,
    I::Item: Send,
    F: Fn(I::Item) -> T + Send + Sync,
    T: Send,
{
    use rayon::iter::ParallelIterator;
    let snap = LogContext::snapshot();
    iter.into_par_iter()
        .map(|item| with_ctx(snap.clone(), || f(item)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_then_drop_restores() {
        LogContext::set_session_id("sid-test".into());
        assert_eq!(LogContext::snapshot().video_path, None);
        {
            let _g = LogContext::enter(LogContextUpdate::default().video_path("a.mp4").op("video.load"));
            let s = LogContext::snapshot();
            assert_eq!(s.session_id, "sid-test");
            assert_eq!(s.video_path.as_deref(), Some("a.mp4"));
            assert_eq!(s.op.as_deref(), Some("video.load"));
        }
        let s = LogContext::snapshot();
        assert_eq!(s.video_path, None);
        assert_eq!(s.op, None);
        // session_id persists
        assert_eq!(s.session_id, "sid-test");
    }

    #[test]
    fn nested_scopes() {
        LogContext::set_session_id("sid-test".into());
        let _outer = LogContext::enter(LogContextUpdate::default().video_path("outer.mp4").op("op-a"));
        {
            let _inner = LogContext::enter(LogContextUpdate::default().op("op-b"));
            let s = LogContext::snapshot();
            assert_eq!(s.video_path.as_deref(), Some("outer.mp4")); // inherited
            assert_eq!(s.op.as_deref(), Some("op-b"));
        }
        let s = LogContext::snapshot();
        assert_eq!(s.op.as_deref(), Some("op-a"));
    }

    #[test]
    fn clear_field() {
        let _g = LogContext::enter(LogContextUpdate::default().video_path("x.mp4"));
        assert_eq!(LogContext::snapshot().video_path.as_deref(), Some("x.mp4"));
        let _g2 = LogContext::enter(LogContextUpdate::default().clear_video_path());
        assert_eq!(LogContext::snapshot().video_path, None);
    }

    #[test]
    fn par_with_ctx_propagates() {
        LogContext::set_session_id("sid-par".into());
        let _g = LogContext::enter(LogContextUpdate::default().op("sync"));
        let results = par_with_ctx(0..16, |i| {
            let s = LogContext::snapshot();
            (i, s.session_id.clone(), s.op.clone())
        });
        assert_eq!(results.len(), 16);
        for (_, sid, op) in &results {
            assert_eq!(sid, "sid-par");
            assert_eq!(op.as_deref(), Some("sync"));
        }
    }

    #[test]
    fn par_with_ctx_does_not_leak_into_main() {
        LogContext::set_session_id("sid-leak".into());
        let _g = LogContext::enter(LogContextUpdate::default().op("outer"));
        let _ = par_with_ctx(0..4, |_| {
            // Worker overwrites op via a nested scope (simulating real code).
            let _s = LogContext::enter(LogContextUpdate::default().op("inner"));
            LogContext::snapshot().op
        });
        // Main thread's context should be unchanged.
        assert_eq!(LogContext::snapshot().op.as_deref(), Some("outer"));
    }
}
