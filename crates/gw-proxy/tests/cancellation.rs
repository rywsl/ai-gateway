use gw_proxy::cancellation::{observe_cancel, CancelCause};
use gw_proxy::ctx::ReqCtx;

#[test]
fn cancel_cause_metric_labels() {
    assert_eq!(CancelCause::ClientClose.as_metric_label(), "client_close");
    assert_eq!(CancelCause::Deadline.as_metric_label(), "deadline");
    assert_eq!(CancelCause::UpstreamErr.as_metric_label(), "upstream_err");
    assert_eq!(CancelCause::RouteAbort.as_metric_label(), "route_abort");
}

#[test]
fn observe_cancel_records_first_cause() {
    let mut ctx = ReqCtx::default();
    assert!(ctx.cancel_cause.is_none());
    observe_cancel(&mut ctx, CancelCause::ClientClose);
    assert!(matches!(ctx.cancel_cause, Some(CancelCause::ClientClose)));
    assert!(ctx.cancel_observed_at_ms.is_some());
}

#[test]
fn observe_cancel_is_idempotent_first_writer_wins() {
    let mut ctx = ReqCtx::default();
    observe_cancel(&mut ctx, CancelCause::ClientClose);
    let first_ts = ctx.cancel_observed_at_ms;
    observe_cancel(&mut ctx, CancelCause::Deadline);
    // First-writer-wins: cause stays as ClientClose, timestamp unchanged
    assert!(matches!(ctx.cancel_cause, Some(CancelCause::ClientClose)));
    assert_eq!(ctx.cancel_observed_at_ms, first_ts);
}
