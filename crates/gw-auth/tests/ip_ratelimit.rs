use gw_auth::ip_ratelimit::IpRateLimitGuard;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn ip(n: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 0, 2, n))
}

#[test]
fn check_allows_requests_within_per_ip_rpm_budget() {
    let guard = IpRateLimitGuard::new(3, 100_000);
    let client_ip = ip(1);

    guard.check(client_ip).expect("first request should pass");
    guard.check(client_ip).expect("second request should pass");
    guard.check(client_ip).expect("third request should pass");
}

#[test]
fn check_rejects_when_per_ip_rpm_budget_is_exhausted() {
    let guard = IpRateLimitGuard::new(1, 100_000);
    let client_ip = ip(2);

    guard.check(client_ip).expect("first request should pass");
    let err = guard.check(client_ip).unwrap_err();

    assert_eq!(err.status, 429);
    assert_eq!(&*err.code, "GW_QUOTA_IP_RPM_EXCEEDED");
    assert_eq!(err.retry_after_secs, Some(60));
    assert!(!err.retryable);
}

#[test]
fn limits_are_tracked_independently_per_ip() {
    let guard = IpRateLimitGuard::new(1, 100_000);

    guard.check(ip(3)).expect("ip A first request passes");
    assert!(guard.check(ip(3)).is_err());
    guard.check(ip(4)).expect("ip B has a separate bucket");
}

#[test]
fn zero_rpm_is_clamped_to_one_request_per_minute() {
    let guard = IpRateLimitGuard::new(0, 100_000);
    let client_ip = ip(5);

    guard
        .check(client_ip)
        .expect("clamped first request passes");
    assert!(guard.check(client_ip).is_err());
}

#[test]
fn sweep_evicts_oldest_entries_when_cap_is_exceeded() {
    let guard = IpRateLimitGuard::new(10, 2);

    guard.check(ip(6)).unwrap();
    guard.check(ip(7)).unwrap();
    guard.check(ip(8)).unwrap();

    let removed = guard.sweep();

    assert_eq!(removed, 1);
    assert_eq!(guard.len(), 2);
    guard
        .check(ip(6))
        .expect("oldest entry was evicted and rebuilt");
}

#[test]
fn ipv4_and_ipv6_addresses_have_separate_buckets() {
    let guard = IpRateLimitGuard::new(1, 100_000);
    let ipv4 = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));
    let ipv6 = IpAddr::V6(Ipv6Addr::LOCALHOST);

    guard.check(ipv4).expect("ipv4 first request passes");
    assert!(guard.check(ipv4).is_err());
    guard.check(ipv6).expect("ipv6 has a separate bucket");
}
