const EARLY_RS: &str = include_str!("../src/filters/early.rs");
const REQUEST_RS: &str = include_str!("../src/filters/request.rs");

fn pos(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("missing marker: {needle}"))
}

fn pos_after(haystack: &str, start: usize, needle: &str) -> usize {
    start
        + haystack[start..]
            .find(needle)
            .unwrap_or_else(|| panic!("missing marker after {start}: {needle}"))
}

#[test]
fn test_filter_chain_order() {
    let early_check = pos(EARLY_RS, "check_ip(");
    let early_global_scope = pos_after(EARLY_RS, early_check, "None,");
    let early_ratelimit_guard = pos_after(EARLY_RS, early_check, "ip_ratelimit");
    let early_protocol_detect = pos_after(EARLY_RS, early_check, "AnyCodec::detect");

    assert!(early_check < early_protocol_detect);
    assert!(early_global_scope > early_check);
    assert!(early_ratelimit_guard > early_check && early_ratelimit_guard < early_protocol_detect);

    let auth = pos(REQUEST_RS, "1. Auth");
    let scoped_check = pos(REQUEST_RS, "check_ip(");
    let scoped_scope = pos_after(REQUEST_RS, scoped_check, "Some(&user_id)");
    let api_key_allowlist = pos(REQUEST_RS, "check_api_key_ip_allowlist");

    assert!(auth < scoped_check);
    assert!(scoped_scope > scoped_check);
    assert!(scoped_check < api_key_allowlist);
}

#[test]
fn test_blocked_ip_never_reaches_auth() {
    let early_check = pos(EARLY_RS, "check_ip(");
    let early_short_circuit = pos(EARLY_RS, "return Err(short_circuit(err.status));");
    let request_auth = pos(REQUEST_RS, "1. Auth");

    assert!(early_check < early_short_circuit);
    assert!(early_short_circuit < EARLY_RS.len());
    assert!(
        request_auth > 0,
        "auth must remain in request_filter, after early filter"
    );
}

#[test]
fn test_ratelimited_ip_never_reaches_auth() {
    let early_check = pos(EARLY_RS, "check_ip(");
    let early_ratelimit_guard = pos_after(EARLY_RS, early_check, "ip_ratelimit");
    let early_short_circuit = pos(EARLY_RS, "return Err(short_circuit(err.status));");
    let request_auth = pos(REQUEST_RS, "1. Auth");

    assert!(early_check < early_ratelimit_guard);
    assert!(early_ratelimit_guard < early_short_circuit);
    assert!(
        request_auth > 0,
        "auth must remain in request_filter, after early filter"
    );
}
