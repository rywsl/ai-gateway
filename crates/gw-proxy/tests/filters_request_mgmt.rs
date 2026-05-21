//! Smoke test: confirm mgmt path detection + parsing helpers from gw-auth
//! resolve the way `filters::request` expects. Real Pingora session
//! integration tests live in e2e_failure_modes.rs; this is a unit smoke.

use gw_auth::mgmt::{is_management_path, parse_mgmt_authorization};

#[test]
fn mgmt_path_combined() {
    // Positive matches: anything under /admin/ is mgmt.
    assert!(is_management_path("/admin/freeze"));
    assert!(is_management_path("/admin/circuits/all"));

    // Negative matches: bare /admin, business paths.
    assert!(!is_management_path("/admin"));
    assert!(!is_management_path("/v1/messages"));

    // Authorization header parsing: only `Mgmt <token>` is accepted.
    assert!(parse_mgmt_authorization("Mgmt foo").is_some());
    assert!(parse_mgmt_authorization("Bearer foo").is_none());
    assert!(parse_mgmt_authorization("").is_none());
}
