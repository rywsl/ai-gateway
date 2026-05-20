//! Asserts the upstream filter's envelope builder produces the right
//! ErrorIR for each user-visible short-circuit code.

use gw_core::error::{codes, ErrorCategory};
use gw_proxy::filters::upstream::envelope_for;

#[test]
fn no_model_group_maps_to_backend_unavailable_503() {
    let err = envelope_for(codes::GW_BACKEND_UNAVAILABLE, "no model_group configured", 503);
    assert_eq!(err.code, codes::GW_BACKEND_UNAVAILABLE);
    assert_eq!(err.status, 503);
    assert_eq!(err.category, ErrorCategory::UpstreamError);
    assert_eq!(err.message, "no model_group configured");
    assert_eq!(err.retry_after_secs, Some(5));
}

#[test]
fn routing_no_channel_maps_to_503() {
    let err = envelope_for(codes::GW_ROUTING_NO_AVAILABLE_CHANNEL, "no candidate", 503);
    assert_eq!(err.code, codes::GW_ROUTING_NO_AVAILABLE_CHANNEL);
    assert_eq!(err.status, 503);
    assert!(err.retryable);
}
