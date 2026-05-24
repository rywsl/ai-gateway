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
//!   the filter returns `Err(Custom("response_deadline_exceeded"))` to abort
//!   the upstream stream.
//! * **Upstream RST / 5xx (W3-T4)**: pending.
//! * **Grace timer (W3-T5)**: pending.

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
/// and returns `true`. Caller is expected to abort the upstream stream
/// (typically by returning a `pingora::Error`).
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
