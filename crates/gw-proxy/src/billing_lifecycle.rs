//! BillingTransaction state machine. Spec §4.1.
//!
//! The 5-state `VendorExposure` enum tracks how far the request has progressed
//! against the upstream vendor. `Cancelling` is intentionally absent — cancel
//! grace handling is internal to `UpstreamCancellationCoordinator` (added in
//! W3), the public state machine exposes only `Streaming → Terminated`.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VendorExposure {
    #[default]
    Idle,
    Sent,
    Streaming,
    Settled,
    Terminated,
}

pub fn can_transition(from: VendorExposure, to: VendorExposure) -> bool {
    use VendorExposure::*;
    matches!(
        (from, to),
        (Idle, Sent)
            | (Sent, Streaming)
            | (Sent, Terminated)
            | (Streaming, Settled)
            | (Streaming, Terminated)
    )
}

pub fn is_terminal(v: VendorExposure) -> bool {
    matches!(v, VendorExposure::Settled | VendorExposure::Terminated)
}

use crate::ctx::ReqCtx;

/// Apply a `VendorExposure` state transition to the ReqCtx.
///
/// Transitions are validated against [`can_transition`]; in debug builds an
/// invalid transition panics. In release, an invalid transition is logged at
/// warn level and the state change is still applied (defensive: don't crash
/// production on a bug we have a way to recover from). Same-state transitions
/// are tolerated (idempotent — no timestamp churn).
///
/// Records `Timing::sent_ms` on first reach of `Sent` and
/// `Timing::settled_ms` on first reach of `Settled` or `Terminated`.
/// Emits the `gw_billing_exposure_transitions_total{from,to}` counter.
pub fn transition_to(ctx: &mut ReqCtx, to: VendorExposure) {
    let from = ctx.vendor_exposure;
    if from == to {
        return;
    }
    if !can_transition(from, to) {
        debug_assert!(false, "invalid transition {:?} -> {:?}", from, to);
        tracing::warn!(
            ?from,
            ?to,
            "invalid billing-lifecycle transition; applying anyway"
        );
    }
    let now_ms = || time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
    match to {
        VendorExposure::Sent if ctx.timing.sent_ms.is_none() => {
            ctx.timing.sent_ms = Some(now_ms());
        }
        VendorExposure::Settled | VendorExposure::Terminated if ctx.timing.settled_ms.is_none() => {
            ctx.timing.settled_ms = Some(now_ms());
        }
        _ => {}
    }
    ctx.vendor_exposure = to;
    metrics::counter!(
        "gw_billing_exposure_transitions_total",
        "from" => format!("{:?}", from).to_lowercase(),
        "to" => format!("{:?}", to).to_lowercase(),
    )
    .increment(1);
}
