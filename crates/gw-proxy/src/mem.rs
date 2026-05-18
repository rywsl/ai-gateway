//! jemalloc thread-local allocation counter sampling.
//!
//! Used by [`ctx::ReqCtx::phase_start`] and `phase_end` to record per-phase
//! allocation deltas in the debug `TraceBuffer`. Returns 0 on any error so the
//! happy path is allocation-free and infallible.

use tikv_jemalloc_ctl::thread;

/// Read the current thread's `allocated` counter from jemalloc.
///
/// Returns 0 if the mib lookup or read fails — debug telemetry is best-effort
/// and must never panic on a hot filter path.
#[inline(always)]
pub fn sample_thread_alloc() -> u64 {
    thread::allocatedp::mib()
        .ok()
        .and_then(|mib| mib.read().ok())
        .map(|tp| tp.get())
        .unwrap_or(0)
}
