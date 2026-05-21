//! Tests for [`gw_auth::merge::merge_permissions`] covering ADR-023
//! five-tier intersection, RPM min-across-tiers, and sub-account effective id.

use gw_auth::{merge_permissions, AuthCtx};
use gw_core::entity::*;
use gw_core::ids::*;
use gw_core::versioned::Versioned;
use gw_store::EntityStore;
use std::net::IpAddr;
use std::sync::Arc;

fn user(
    id: &str,
    parent: Option<&str>,
    models: &[&str],
    rpm: u32,
    groups: &[&str],
    markup: f32,
) -> Arc<Versioned<UserEntry>> {
    Arc::new(Versioned::new(
        1,
        UserEntry {
            user_id: UserId::from(id),
            parent_id: parent.map(UserId::from),
            tier: "standard".into(),
            status: UserStatus::Active,
            allowed_models: models.iter().copied().map(ModelName::from).collect(),
            allowed_channels: vec![],
            allowed_groups: vec![],
            rpm_limit: rpm,
            tpm_limit: 0,
            max_concurrent_streams: 100,
            daily_quota_tokens: 0,
            user_group_ids: groups.iter().copied().map(UserGroupId::from).collect(),
            markup,
            debug_enabled: false,
        },
    ))
}

fn group(id: &str, models: &[&str], rpm: u32, markup: f32) -> Arc<Versioned<UserGroup>> {
    Arc::new(Versioned::new(
        1,
        UserGroup {
            id: UserGroupId::from(id),
            display_name: id.to_string(),
            priority: 0,
            allowed_models: models.iter().copied().map(ModelName::from).collect(),
            allowed_channels: vec![],
            model_group_binding: None,
            rpm_limit: rpm,
            tpm_limit: 0,
            max_concurrent_streams: 0,
            markup,
        },
    ))
}

fn apikey(
    id: &str,
    user_id: &str,
    models: &[&str],
    rpm: u32,
    markup: f32,
) -> Arc<Versioned<ApiKeyEntry>> {
    Arc::new(Versioned::new(
        1,
        ApiKeyEntry {
            key_id: ApiKeyId::from(id),
            user_id: UserId::from(user_id),
            status: ApiKeyStatus::Active,
            hash: vec![0; 32],
            allowed_models: models.iter().copied().map(ModelName::from).collect(),
            allowed_channels: vec![],
            rpm_limit: rpm,
            tpm_limit: 0,
            markup,
            ip_allowlist: None,
            expires_at: None,
            signature_required: false,
            signature_algo: SignatureAlgo::default(),
            force_capabilities: smallvec::SmallVec::new(),
            match_mode_exclusive: false,
        },
    ))
}

fn ip() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

#[tokio::test]
async fn three_way_model_intersection() {
    // user allows {gpt-4, gpt-3.5, claude}
    // group allows {gpt-4, gpt-3.5}
    // apikey allows {gpt-4}
    // expected: {gpt-4}
    let store = EntityStore::new();
    store
        .user
        .insert(
            UserId::from("u1"),
            user("u1", None, &["gpt-4", "gpt-3.5", "claude"], 0, &["g1"], 1.0),
        )
        .await;
    store
        .user_group
        .insert(
            UserGroupId::from("g1"),
            group("g1", &["gpt-4", "gpt-3.5"], 0, 1.0),
        )
        .await;
    store
        .apikey
        .insert(
            ApiKeyId::from("ak1"),
            apikey("ak1", "u1", &["gpt-4"], 0, 1.0),
        )
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: Some(ApiKeyId::from("ak1")),
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 0).await.unwrap();

    assert_eq!(perm.allowed_models.len(), 1);
    assert!(perm.allowed_models.contains(&ModelName::from("gpt-4")));
}

#[tokio::test]
async fn rpm_takes_min_across_tiers() {
    // user=100, group=50, apikey=200, global=300 -> min positive = 50
    let store = EntityStore::new();
    store
        .user
        .insert(
            UserId::from("u1"),
            user("u1", None, &[], 100, &["g1"], 1.0),
        )
        .await;
    store
        .user_group
        .insert(UserGroupId::from("g1"), group("g1", &[], 50, 1.0))
        .await;
    store
        .apikey
        .insert(
            ApiKeyId::from("ak1"),
            apikey("ak1", "u1", &[], 200, 1.0),
        )
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: Some(ApiKeyId::from("ak1")),
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 300).await.unwrap();
    assert_eq!(perm.rpm_limit, 50);
}

#[tokio::test]
async fn sub_account_effective_user_id_is_parent() {
    let store = EntityStore::new();
    store
        .user
        .insert(UserId::from("parent"), user("parent", None, &[], 0, &[], 1.0))
        .await;
    store
        .user
        .insert(
            UserId::from("child"),
            user("child", Some("parent"), &[], 0, &[], 1.0),
        )
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("child"),
        api_key_id: None,
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 0).await.unwrap();
    assert_eq!(perm.user_id, UserId::from("child"));
    assert_eq!(perm.effective_user_id, UserId::from("parent"));
}

#[tokio::test]
async fn markup_is_product_of_tiers() {
    let store = EntityStore::new();
    store
        .user
        .insert(UserId::from("u1"), user("u1", None, &[], 0, &["g1"], 1.5))
        .await;
    store
        .user_group
        .insert(UserGroupId::from("g1"), group("g1", &[], 0, 2.0))
        .await;
    store
        .apikey
        .insert(
            ApiKeyId::from("ak1"),
            apikey("ak1", "u1", &[], 0, 3.0),
        )
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: Some(ApiKeyId::from("ak1")),
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 0).await.unwrap();
    // user * main(==user) * group * apikey = 1.5 * 1.5 * 2.0 * 3.0 = 13.5
    assert!((perm.markup_chain.product - 13.5).abs() < 1e-4);
}
