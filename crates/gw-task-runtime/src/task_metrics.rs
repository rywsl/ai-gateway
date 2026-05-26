//! Worker-side task metrics (ADR-T9).
//!
//! Constants live here so call sites use the same metric name strings.
//! Registration happens in [`gw-gateway::metrics_catalog`] (gateway process)
//! and via `describe_worker_metrics` (worker process on :9091).

use metrics::Unit;

pub const TASK_CLAIMED_TOTAL: &str = "gw_task_claimed_total";
pub const TASK_SETTLED_TOTAL: &str = "gw_task_settled_total";
pub const TASK_EXECUTION_DURATION_SECONDS: &str = "gw_task_execution_duration_seconds";
pub const TASK_LEASE_RECOVERED_TOTAL: &str = "gw_task_lease_recovered_total";
pub const TASK_HEARTBEAT_TOTAL: &str = "gw_task_heartbeat_total";
pub const TASK_CANCEL_RECEIVED_TOTAL: &str = "gw_task_cancel_received_total";

/// Register worker-side metric descriptions so they appear in /metrics before
/// first traffic. Called by `gw-task-worker` at startup.
pub fn describe_worker_metrics() {
    metrics::describe_counter!(
        TASK_CLAIMED_TOTAL,
        Unit::Count,
        "Tasks claimed from the queue by this worker"
    );
    metrics::describe_counter!(
        TASK_SETTLED_TOTAL,
        Unit::Count,
        "Tasks settled by this worker; label=status (succeeded/failed/cancelled)"
    );
    metrics::describe_histogram!(
        TASK_EXECUTION_DURATION_SECONDS,
        "Task execution wall-clock duration in seconds"
    );
    metrics::describe_counter!(
        TASK_LEASE_RECOVERED_TOTAL,
        Unit::Count,
        "Expired task leases recovered by the watchdog"
    );
    metrics::describe_counter!(
        TASK_HEARTBEAT_TOTAL,
        Unit::Count,
        "Task lease heartbeat renewals sent"
    );
    metrics::describe_counter!(
        TASK_CANCEL_RECEIVED_TOTAL,
        Unit::Count,
        "Cancel events received and processed by the cancel loop"
    );
}
