//! G2 — per-key ip_allowlist integration test.
//!
//! Exercises the pure `check_api_key_ip_allowlist` helper that `request_filter`
//! calls right after the User/ApiKey-scoped blocklist re-check. Spinning up
//! Pingora in a unit test is expensive, so — mirroring T1-A's expires_at test —
//! we test the helper directly. Inputs match what the call site passes:
//! `entry.value.ip_allowlist.as_deref()` and the resolved client IP.
use gw_proxy::filters::request::check_api_key_ip_allowlist;
use std::net::IpAddr;

#[test]
fn none_means_unrestricted() {
    let ip: IpAddr = "1.2.3.4".parse().unwrap();
    assert!(check_api_key_ip_allowlist(None, ip).is_ok());
}

#[test]
fn empty_list_means_unrestricted() {
    let ip: IpAddr = "1.2.3.4".parse().unwrap();
    let empty: Vec<String> = vec![];
    assert!(check_api_key_ip_allowlist(Some(empty.as_slice()), ip).is_ok());
}

#[test]
fn matching_cidr_passes() {
    let ip: IpAddr = "10.0.0.5".parse().unwrap();
    let list = vec!["10.0.0.0/24".to_string()];
    assert!(check_api_key_ip_allowlist(Some(list.as_slice()), ip).is_ok());
}

#[test]
fn nonmatching_cidr_rejects() {
    let ip: IpAddr = "192.168.1.1".parse().unwrap();
    let list = vec!["10.0.0.0/24".to_string()];
    assert!(check_api_key_ip_allowlist(Some(list.as_slice()), ip).is_err());
}
