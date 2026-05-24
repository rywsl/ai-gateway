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

#[test]
fn client_close_error_kinds_map_to_client_close() {
    use std::io::ErrorKind;
    for kind in [
        ErrorKind::BrokenPipe,
        ErrorKind::ConnectionReset,
        ErrorKind::ConnectionAborted,
        ErrorKind::UnexpectedEof,
    ] {
        assert!(
            gw_proxy::cancellation::io_error_kind_is_client_close(kind),
            "expected {:?} to map to client_close",
            kind
        );
    }
    for kind in [ErrorKind::PermissionDenied, ErrorKind::NotFound] {
        assert!(
            !gw_proxy::cancellation::io_error_kind_is_client_close(kind),
            "expected {:?} NOT to map to client_close",
            kind
        );
    }
}

#[test]
fn observe_client_close_records_cause() {
    use gw_proxy::cancellation::{observe_client_close_for_io, CancelCause};
    use gw_proxy::ctx::ReqCtx;
    let mut ctx = ReqCtx::default();
    observe_client_close_for_io(&mut ctx, std::io::ErrorKind::BrokenPipe);
    assert!(matches!(ctx.cancel_cause, Some(CancelCause::ClientClose)));
}

#[test]
fn non_client_close_kind_does_not_observe_cancel() {
    use gw_proxy::cancellation::observe_client_close_for_io;
    use gw_proxy::ctx::ReqCtx;
    let mut ctx = ReqCtx::default();
    observe_client_close_for_io(&mut ctx, std::io::ErrorKind::PermissionDenied);
    assert!(ctx.cancel_cause.is_none());
}

#[test]
fn deadline_helper_marks_when_exceeded() {
    use gw_proxy::cancellation::{check_response_deadline, CancelCause};
    use gw_proxy::ctx::ReqCtx;
    let mut ctx = ReqCtx::default();
    // sent 1000 ms ago
    let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
    ctx.timing.sent_ms = Some(now_ms - 1000);
    // deadline 500ms — should fire
    let exceeded = check_response_deadline(&mut ctx, 500, now_ms);
    assert!(exceeded);
    assert!(matches!(ctx.cancel_cause, Some(CancelCause::Deadline)));
}

#[test]
fn deadline_helper_does_not_mark_when_within_budget() {
    use gw_proxy::cancellation::check_response_deadline;
    use gw_proxy::ctx::ReqCtx;
    let mut ctx = ReqCtx::default();
    let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
    ctx.timing.sent_ms = Some(now_ms - 100);
    // deadline 500ms — within budget
    let exceeded = check_response_deadline(&mut ctx, 500, now_ms);
    assert!(!exceeded);
    assert!(ctx.cancel_cause.is_none());
}

#[test]
fn deadline_helper_no_sent_ms_is_noop() {
    use gw_proxy::cancellation::check_response_deadline;
    use gw_proxy::ctx::ReqCtx;
    let mut ctx = ReqCtx::default();
    let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
    let exceeded = check_response_deadline(&mut ctx, 500, now_ms);
    assert!(!exceeded);
    assert!(ctx.cancel_cause.is_none());
}
