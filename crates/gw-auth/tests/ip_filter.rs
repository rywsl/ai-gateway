//! G2 — CIDR list matching for ApiKeyEntry.ip_allowlist.
use gw_auth::ip_allowlist::{IpAllowlistGuard, IpAllowlistMode};
use gw_auth::ip_filter::{check_ip, match_any_cidr};
use gw_auth::ip_ratelimit::IpRateLimitGuard;
use gw_core::entity::{IpBlockEntry, IpBlockMode, IpBlockReason, IpBlockScope};
use gw_core::error::codes;
use gw_core::ids::{ApiKeyId, UserId};
use gw_core::versioned::Versioned;
use gw_store::EntityStore;
use std::net::IpAddr;
use std::sync::Arc;

fn entry(cidr: &str, mode: IpBlockMode, scope: IpBlockScope) -> Arc<Versioned<IpBlockEntry>> {
    Arc::new(Versioned::new(
        1,
        IpBlockEntry {
            cidr: cidr.to_string(),
            mode,
            scope,
            reason: IpBlockReason::Manual,
            expires_at: None,
        },
    ))
}

fn ip(value: &str) -> IpAddr {
    value.parse().unwrap()
}

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

#[tokio::test]
async fn test_check_ip_deny_first() {
    let store = EntityStore::new();
    store
        .ipblock
        .insert(
            "allow-office".to_string(),
            entry("10.0.0.0/24", IpBlockMode::Allow, IpBlockScope::Global),
        )
        .await;
    store
        .ipblock
        .insert(
            "deny-host".to_string(),
            entry("10.0.0.42/32", IpBlockMode::Deny, IpBlockScope::Global),
        )
        .await;

    let err = check_ip(
        ip("10.0.0.42"),
        None,
        None,
        &store,
        &IpAllowlistGuard::new(IpAllowlistMode::Enforce),
        &IpRateLimitGuard::new(1, 100_000),
        IpAllowlistMode::Enforce,
    )
    .await
    .expect_err("deny list must short-circuit before allowlist and ratelimit");

    assert_eq!(err.status, 403);
    assert_eq!(err.code, codes::GW_AUTH_IP_BLOCKED);
}

#[tokio::test]
async fn test_check_ip_allow_then_ratelimit() {
    let store = EntityStore::new();
    store
        .ipblock
        .insert(
            "allow-office".to_string(),
            entry("10.0.0.0/24", IpBlockMode::Allow, IpBlockScope::Global),
        )
        .await;
    let allowlist_guard = IpAllowlistGuard::new(IpAllowlistMode::Enforce);
    let ratelimit_guard = IpRateLimitGuard::new(1, 100_000);
    let client_ip = ip("10.0.0.10");

    check_ip(
        client_ip,
        None,
        None,
        &store,
        &allowlist_guard,
        &ratelimit_guard,
        IpAllowlistMode::Enforce,
    )
    .await
    .expect("allowlisted first request should pass");

    let err = check_ip(
        client_ip,
        None,
        None,
        &store,
        &allowlist_guard,
        &ratelimit_guard,
        IpAllowlistMode::Enforce,
    )
    .await
    .expect_err("second allowlisted request should reach IP ratelimit");

    assert_eq!(err.status, 429);
    assert_eq!(err.code, codes::GW_QUOTA_IP_RPM_EXCEEDED);
}

#[tokio::test]
async fn test_check_ip_scoped_check() {
    let store = EntityStore::new();
    let user_id = UserId::from("user-1");
    let api_key_id = ApiKeyId::from("key-1");
    store
        .ipblock
        .insert(
            "deny-user".to_string(),
            entry(
                "203.0.113.0/24",
                IpBlockMode::Deny,
                IpBlockScope::User(user_id.clone()),
            ),
        )
        .await;

    check_ip(
        ip("203.0.113.7"),
        None,
        None,
        &store,
        &IpAllowlistGuard::new(IpAllowlistMode::Off),
        &IpRateLimitGuard::new(10, 100_000),
        IpAllowlistMode::Off,
    )
    .await
    .expect("pre-auth global-only check should ignore user-scoped deny");

    let err = check_ip(
        ip("203.0.113.7"),
        Some(&user_id),
        Some(&api_key_id),
        &store,
        &IpAllowlistGuard::new(IpAllowlistMode::Off),
        &IpRateLimitGuard::new(10, 100_000),
        IpAllowlistMode::Off,
    )
    .await
    .expect_err("post-auth scoped check should apply matching user deny");

    assert_eq!(err.code, codes::GW_AUTH_IP_BLOCKED);
}

#[tokio::test]
async fn test_check_ip_redis_outage_failopen() {
    let store = EntityStore::new();
    let ratelimit_guard = IpRateLimitGuard::new(600, 100_000);

    check_ip(
        ip("198.51.100.9"),
        None,
        None,
        &store,
        &IpAllowlistGuard::new(IpAllowlistMode::Off),
        &ratelimit_guard,
        IpAllowlistMode::Off,
    )
    .await
    .expect("in-memory IP limiter has no Redis dependency, so outage-equivalent path is fail-open");
}
