//! UpstreamCancellationCoordinator — uniform cancel-source detection.
//!
//! Spec §5.3. This module provides a [`CancelCause`] enum and the
//! [`observe_cancel`] helper. Filter-side detection landed as follows:
//!
//! * **Client disconnect (W3-T2)**: classified in `LlmGateway::logging` by
//!   matching `pingora::Error { esource: Downstream, etype: ConnectionClosed
//!   | WriteError | ReadError }`. The spec §5.3 table cited
//!   `response_body_filter` as the detection site, but Pingora 0.8 doesn't
//!   expose a stable per-chunk disconnect check from that filter — the
//!   `logging` classifier reaches the same outcome (a `ClientClose` cancel
//!   cause on `ctx` before the UsageRecord is emitted).
//! * **Response deadline (W3-T3)**: `check_response_deadline` called in
//!   `LlmGateway::response_body_filter` on each chunk. If
//!   `now_ms - ctx.timing.sent_ms > response_deadline_ms` the helper fires
//!   [`observe_cancel`] with [`CancelCause::Deadline`] and returns `true`;
//!   the filter enters grace mode via [`check_grace_status`] instead of
//!   aborting immediately.
//! * **Upstream RST / 5xx (W3-T4)**: classified in `logging`.
//! * **Grace timer (W3-T5)**: `check_grace_status` / `record_grace_outcome`.

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

/// Grace window status for cancel-aware body reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraceStatus {
    /// No cancel observed yet — body reads proceed normally.
    NotApplicable,
    /// Cancel observed within grace window — body reads continue to chase
    /// final usage frames.
    Active,
    /// Grace window expired — caller should abort the body read.
    Expired,
}

/// Check whether we're inside the cancel grace window. Sets / clears
/// `ctx.cancel_grace_active` so the response filter can short-circuit.
pub fn check_grace_status(ctx: &mut ReqCtx, grace_ms: u64, now_ms: i64) -> GraceStatus {
    let Some(cancel_at) = ctx.cancel_observed_at_ms else {
        ctx.cancel_grace_active = false;
        return GraceStatus::NotApplicable;
    };
    let elapsed = now_ms.saturating_sub(cancel_at);
    if elapsed <= grace_ms as i64 {
        ctx.cancel_grace_active = true;
        GraceStatus::Active
    } else {
        ctx.cancel_grace_active = false;
        GraceStatus::Expired
    }
}

/// Record whether the grace window captured a terminal extraction.
///
/// Emits `gw_billing_grace_yielded_final_usage_total{yielded=true|false}`.
pub fn record_grace_outcome(yielded: bool) {
    let label = if yielded { "true" } else { "false" };
    metrics::counter!(
        "gw_billing_grace_yielded_final_usage_total",
        "yielded" => label,
    )
    .increment(1);
}

/// True for [`std::io::ErrorKind`] variants that strongly indicate a
/// client-side disconnect on the downstream HTTP/1.1 or HTTP/2 stream.
pub fn io_error_kind_is_client_close(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        kind,
        BrokenPipe | ConnectionReset | ConnectionAborted | UnexpectedEof
    )
}

/// Observe a [`CancelCause::ClientClose`] cancellation if `kind` indicates
/// one. No-op otherwise. Wraps [`observe_cancel`] with the io::ErrorKind
/// filter.
pub fn observe_client_close_for_io(ctx: &mut ReqCtx, kind: std::io::ErrorKind) {
    if io_error_kind_is_client_close(kind) {
        observe_cancel(ctx, CancelCause::ClientClose);
    }
}

/// Check whether the request has exceeded `deadline_ms` since `Sent`.
/// On exceedance, calls [`observe_cancel`] with [`CancelCause::Deadline`]
/// and returns `true`. Caller is expected to enter grace mode (via
/// [`check_grace_status`]) rather than aborting immediately.
///
/// Returns `false` if `ctx.timing.sent_ms` is not yet set (request hasn't
/// reached upstream).
pub fn check_response_deadline(ctx: &mut ReqCtx, deadline_ms: u64, now_ms: i64) -> bool {
    let Some(sent_ms) = ctx.timing.sent_ms else {
        return false;
    };
    let elapsed = now_ms.saturating_sub(sent_ms);
    if elapsed > deadline_ms as i64 {
        observe_cancel(ctx, CancelCause::Deadline);
        true
    } else {
        false
    }
}

/// Classify a pingora-error inspection result into a `CancelCause`.
/// Caller pre-extracts `is_upstream` / `is_client_close` flags from the Pingora
/// error's `esource` and `etype` fields.
///
/// Precedence: upstream over client (rare for both to be true, but if so,
/// upstream is the more actionable signal for billing). `None` means the
/// error doesn't correspond to a cancel-worthy event.
pub fn pingora_error_to_cancel_cause(
    is_upstream: bool,
    is_client_close: bool,
) -> Option<CancelCause> {
    if is_upstream {
        Some(CancelCause::UpstreamErr)
    } else if is_client_close {
        Some(CancelCause::ClientClose)
    } else {
        None
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
