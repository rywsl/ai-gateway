//! status_class metric correctly buckets short-circuit errors by HTTP status.
//!
//! Builds a synthetic `pingora::Error::because(ErrorType::HTTPStatus(401), ...)`
//! and a minimal ReqCtx with `upstream_status: None`, runs the same matcher
//! used by the logging filter, and asserts the result is "4xx", not "err".

use pingora::{Error, ErrorType};

#[test]
fn short_circuit_401_buckets_as_4xx() {
    let e = Error::because(
        ErrorType::HTTPStatus(401),
        "auth",
        Error::new(ErrorType::Custom("gw_short_circuit")),
    );
    assert_eq!(
        gw_proxy::filters::logging::status_class_for(Some(&e), None),
        "4xx"
    );
}

#[test]
fn short_circuit_413_buckets_as_4xx() {
    let e = Error::because(
        ErrorType::HTTPStatus(413),
        "too large",
        Error::new(ErrorType::Custom("gw_short_circuit")),
    );
    assert_eq!(
        gw_proxy::filters::logging::status_class_for(Some(&e), None),
        "4xx"
    );
}

#[test]
fn upstream_200_buckets_as_2xx() {
    assert_eq!(
        gw_proxy::filters::logging::status_class_for(None, Some(200)),
        "2xx"
    );
}

#[test]
fn unknown_error_no_status_falls_back_to_err() {
    let e = Error::new(ErrorType::Custom("misc"));
    assert_eq!(
        gw_proxy::filters::logging::status_class_for(Some(&e), None),
        "err"
    );
}

#[test]
fn no_err_no_upstream_is_early() {
    assert_eq!(
        gw_proxy::filters::logging::status_class_for(None, None),
        "early"
    );
}
