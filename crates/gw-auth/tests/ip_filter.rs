//! G2 — CIDR list matching for ApiKeyEntry.ip_allowlist.
use gw_auth::ip_filter::match_any_cidr;
use std::net::IpAddr;

#[test]
fn empty_allowlist_returns_false() {
    let ip: IpAddr = "1.2.3.4".parse().unwrap();
    assert!(!match_any_cidr(ip, &[]));
}

#[test]
fn ipv4_prefix_match() {
    let ip: IpAddr = "10.0.0.5".parse().unwrap();
    assert!(match_any_cidr(ip, &["10.0.0.0/24".into()]));
}

#[test]
fn ipv4_no_match() {
    let ip: IpAddr = "192.168.1.1".parse().unwrap();
    assert!(!match_any_cidr(ip, &["10.0.0.0/24".into()]));
}

#[test]
fn ipv4_single_host_match() {
    // Spec ADR-G2-1 acceptance: "IPv4 single-host" coverage. `/32` matches exactly the host.
    let ip: IpAddr = "10.0.0.5".parse().unwrap();
    assert!(match_any_cidr(ip, &["10.0.0.5/32".into()]));
}

#[test]
fn ipv6_prefix_match() {
    let ip: IpAddr = "2001:db8::1".parse().unwrap();
    assert!(match_any_cidr(ip, &["2001:db8::/32".into()]));
}

#[test]
fn malformed_cidr_excluded_silently() {
    // Spec: malformed entries fail closed — they're skipped from the candidate
    // matchers, never auto-match. Single malformed CIDR + no valid match → false.
    let ip: IpAddr = "1.2.3.4".parse().unwrap();
    assert!(!match_any_cidr(ip, &["not-a-cidr".into()]));
}

#[test]
fn mixed_valid_and_malformed_still_matches_valid() {
    // Robustness: a malformed entry does NOT prevent a valid entry from matching.
    let ip: IpAddr = "10.0.0.5".parse().unwrap();
    let list = vec!["not-a-cidr".to_string(), "10.0.0.0/24".to_string()];
    assert!(match_any_cidr(ip, &list));
}
