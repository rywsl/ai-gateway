use gw_auth::ip_allowlist::{IpAllowlistGuard, IpAllowlistMode};
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

#[tokio::test]
async fn allowlist_enforce_blocks_unlisted_ip() {
    let store = EntityStore::new();
    store
        .ipblock
        .insert(
            "allow-office".to_string(),
            entry("10.0.0.0/24", IpBlockMode::Allow, IpBlockScope::Global),
        )
        .await;

    let err = IpAllowlistGuard::new(IpAllowlistMode::Enforce)
        .check(ip("192.0.2.10"), None, None, &store)
        .await
        .expect_err("unlisted IP should be rejected");

    assert_eq!(err.status, 403);
    assert_eq!(err.code, codes::GW_AUTH_IP_NOT_IN_ALLOWLIST);
}

#[tokio::test]
async fn allowlist_enforce_passes_listed_ip() {
    let store = EntityStore::new();
    store
        .ipblock
        .insert(
            "allow-office".to_string(),
            entry("10.0.0.0/24", IpBlockMode::Allow, IpBlockScope::Global),
        )
        .await;

    IpAllowlistGuard::new(IpAllowlistMode::Enforce)
        .check(ip("10.0.0.42"), None, None, &store)
        .await
        .expect("listed IP should pass");
}

#[tokio::test]
async fn allowlist_off_passes_all() {
    let store = EntityStore::new();
    store
        .ipblock
        .insert(
            "allow-office".to_string(),
            entry("10.0.0.0/24", IpBlockMode::Allow, IpBlockScope::Global),
        )
        .await;

    IpAllowlistGuard::new(IpAllowlistMode::Off)
        .check(ip("192.0.2.10"), None, None, &store)
        .await
        .expect("off mode should not enforce allow entries");
}

#[tokio::test]
async fn blocklist_deny_still_works() {
    let store = EntityStore::new();
    store
        .ipblock
        .insert(
            "deny-bad".to_string(),
            entry("203.0.113.0/24", IpBlockMode::Deny, IpBlockScope::Global),
        )
        .await;

    let err = IpAllowlistGuard::new(IpAllowlistMode::Off)
        .check(ip("203.0.113.7"), None, None, &store)
        .await
        .expect_err("deny entry should reject even when allowlist is off");

    assert_eq!(err.status, 403);
    assert_eq!(err.code, codes::GW_AUTH_IP_BLOCKED);
}

#[tokio::test]
async fn deny_takes_priority_over_allow() {
    let store = EntityStore::new();
    store
        .ipblock
        .insert(
            "allow-all-corp".to_string(),
            entry("10.0.0.0/24", IpBlockMode::Allow, IpBlockScope::Global),
        )
        .await;
    store
        .ipblock
        .insert(
            "deny-one-host".to_string(),
            entry("10.0.0.42/32", IpBlockMode::Deny, IpBlockScope::Global),
        )
        .await;

    let err = IpAllowlistGuard::new(IpAllowlistMode::Enforce)
        .check(ip("10.0.0.42"), None, None, &store)
        .await
        .expect_err("matching deny should win over matching allow");

    assert_eq!(err.status, 403);
    assert_eq!(err.code, codes::GW_AUTH_IP_BLOCKED);
}

#[tokio::test]
async fn scoped_allowlist_only_applies_to_matching_identity() {
    let store = EntityStore::new();
    let user_id = UserId::from("user-1");
    let api_key_id = ApiKeyId::from("key-1");
    store
        .ipblock
        .insert(
            "allow-user".to_string(),
            entry(
                "10.0.0.0/24",
                IpBlockMode::Allow,
                IpBlockScope::User(user_id.clone()),
            ),
        )
        .await;

    IpAllowlistGuard::new(IpAllowlistMode::Enforce)
        .check(ip("192.0.2.10"), None, Some(&api_key_id), &store)
        .await
        .expect("non-matching scoped allow entries should not restrict request");
}
