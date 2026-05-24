//! UpstreamCancellationCoordinator — uniform cancel-source detection.
//!
//! Spec §5.3. This module provides a `CancelCause` enum and an
//! `observe_cancel` helper. Actual filter-side detection (TCP close,
//! response deadline, upstream RST) lands in W3-T2/T3/T4. Grace timer
//! handling lands in W3-T5.

use crate::ctx::ReqCtx;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelCause {
    ClientClose,
    Deadline,
    UpstreamErr,
    RouteAbort,
}

impl CancelCause {
    pub fn as_metric_label(&self) -> &'static str {
        match self {
            CancelCause::ClientClose => "client_close",
            CancelCause::Deadline => "deadline",
            CancelCause::UpstreamErr => "upstream_err",
            CancelCause::RouteAbort => "route_abort",
        }
    }
}

/// First-writer-wins. Subsequent observations don't overwrite — the first
/// cancel signal is the one that matters for diagnosing root cause.
pub fn observe_cancel(ctx: &mut ReqCtx, cause: CancelCause) {
    if ctx.cancel_observed_at_ms.is_some() {
        return;
    }
    let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
    ctx.cancel_observed_at_ms = Some(now_ms);
    ctx.cancel_cause = Some(cause);
    metrics::counter!(
        "gw_billing_upstream_cancellation_total",
        "cause" => cause.as_metric_label(),
    )
    .increment(1);
}
