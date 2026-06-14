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
            signature_secret: None,
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
            model_whitelist_regex: None,
        },
    ))
}

fn pricing(model: &str, markup: f32) -> Arc<Versioned<Pricing>> {
    Arc::new(Versioned::new(
        1,
        Pricing {
            model: ModelName::from(model),
            input_price_micro_per_token: 0,
            output_price_micro_per_token: 0,
            max_output: 0,
            model_markup: markup,
            connect_timeout_ms: None,
            read_timeout_ms: None,
            total_timeout_ms: None,
            stream_total_timeout_ms: None,
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
    let perm = merge_permissions(&ctx, &store, 0, None).await.unwrap();

    assert_eq!(perm.allowed_models.len(), 1);
    assert!(perm.allowed_models.contains(&ModelName::from("gpt-4")));
}

#[tokio::test]
async fn rpm_takes_min_across_tiers() {
    // user=100, group=50, apikey=200, global=300 -> min positive = 50
    let store = EntityStore::new();
    store
        .user
        .insert(UserId::from("u1"), user("u1", None, &[], 100, &["g1"], 1.0))
        .await;
    store
        .user_group
        .insert(UserGroupId::from("g1"), group("g1", &[], 50, 1.0))
        .await;
    store
        .apikey
        .insert(ApiKeyId::from("ak1"), apikey("ak1", "u1", &[], 200, 1.0))
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: Some(ApiKeyId::from("ak1")),
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 300, None).await.unwrap();
    assert_eq!(perm.rpm_limit, 50);
}

#[tokio::test]
async fn sub_account_effective_user_id_is_parent() {
    let store = EntityStore::new();
    store
        .user
        .insert(
            UserId::from("parent"),
            user("parent", None, &[], 0, &[], 1.0),
        )
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
    let perm = merge_permissions(&ctx, &store, 0, None).await.unwrap();
    assert_eq!(perm.user_id, UserId::from("child"));
    assert_eq!(perm.effective_user_id, UserId::from("parent"));
}

#[tokio::test]
async fn markup_is_product_of_tiers() {
    let store = EntityStore::new();
    store
        .user
        .insert(UserId::from("u1"), user("u1", None, &[], 0, &["g1"], 1.0))
        .await;
    store
        .user_group
        .insert(UserGroupId::from("g1"), group("g1", &[], 0, 1.10))
        .await;
    store
        .apikey
        .insert(ApiKeyId::from("ak1"), apikey("ak1", "u1", &[], 0, 1.20))
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: Some(ApiKeyId::from("ak1")),
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 0, Some(pricing("gpt-4", 1.10)))
        .await
        .unwrap();
    assert!((perm.markup_chain.user_group_markup - 1.10).abs() < 1e-4);
    assert!((perm.markup_chain.api_key_markup - 1.20).abs() < 1e-4);
    assert!((perm.markup_chain.vendor_markup - 1.0).abs() < 1e-4);
    assert!((perm.markup_chain.model_markup - 1.10).abs() < 1e-4);
    assert!((perm.markup_chain.product - 1.452).abs() < 1e-4);
    let product = perm.markup_chain.user_group_markup
        * perm.markup_chain.api_key_markup
        * perm.markup_chain.vendor_markup
        * perm.markup_chain.model_markup;
    assert!((perm.markup_chain.product - product).abs() < 1e-4);
}

#[tokio::test]
async fn direct_user_markup_is_not_double_counted() {
    let store = EntityStore::new();
    store
        .user
        .insert(UserId::from("u1"), user("u1", None, &[], 0, &[], 1.5))
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: None,
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 0, None).await.unwrap();
    assert!((perm.markup_chain.user_group_markup - 1.5).abs() < 1e-4);
    assert!((perm.markup_chain.product - 1.5).abs() < 1e-4);
}

#[tokio::test]
async fn sub_account_markup_includes_child_and_parent_once() {
    let store = EntityStore::new();
    store
        .user
        .insert(
            UserId::from("parent"),
            user("parent", None, &[], 0, &["g1"], 1.5),
        )
        .await;
    store
        .user
        .insert(
            UserId::from("child"),
            user("child", Some("parent"), &[], 0, &[], 1.2),
        )
        .await;
    store
        .user_group
        .insert(UserGroupId::from("g1"), group("g1", &[], 0, 2.0))
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("child"),
        api_key_id: None,
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 0, None).await.unwrap();
    assert!((perm.markup_chain.user_group_markup - 3.6).abs() < 1e-4);
    assert!((perm.markup_chain.product - 3.6).abs() < 1e-4);
}

// --- Audit 2026-05-30: banned-user / banned-parent admit-time gate ---

fn banned_user(id: &str, parent: Option<&str>) -> Arc<Versioned<UserEntry>> {
    Arc::new(Versioned::new(
        1,
        UserEntry {
            user_id: UserId::from(id),
            parent_id: parent.map(UserId::from),
            tier: "standard".into(),
            status: UserStatus::Banned,
            allowed_models: vec![],
            allowed_channels: vec![],
            allowed_groups: vec![],
            rpm_limit: 0,
            tpm_limit: 0,
            max_concurrent_streams: 100,
            daily_quota_tokens: 0,
            user_group_ids: vec![],
            markup: 1.0,
            debug_enabled: false,
        },
    ))
}

#[tokio::test]
async fn banned_user_is_rejected() {
    let store = EntityStore::new();
    store
        .user
        .insert(UserId::from("u1"), banned_user("u1", None))
        .await;
    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: None,
        client_ip: ip(),
    };
    let err = merge_permissions(&ctx, &store, 0, None)
        .await
        .expect_err("banned user must be rejected");
    assert_eq!(err.code, gw_core::error::codes::GW_AUTH_USER_BANNED);
    assert_eq!(err.status, 403);
}

#[tokio::test]
async fn banned_parent_rejects_active_sub_account() {
    // The sub-account itself is Active but its parent (owner) is Banned. The
    // request_filter only loads the direct user; merge_permissions loads the
    // parent, so this is where a banned owner disables all sub-accounts.
    let store = EntityStore::new();
    store
        .user
        .insert(
            UserId::from("sub"),
            user("sub", Some("owner"), &[], 0, &[], 1.0),
        )
        .await;
    store
        .user
        .insert(UserId::from("owner"), banned_user("owner", None))
        .await;
    let ctx = AuthCtx {
        user_id: UserId::from("sub"),
        api_key_id: None,
        client_ip: ip(),
    };
    let err = merge_permissions(&ctx, &store, 0, None)
        .await
        .expect_err("active sub-account of a banned owner must be rejected");
    assert_eq!(err.code, gw_core::error::codes::GW_AUTH_USER_BANNED);
}

// --- P0 #16: dedicated user-not-found code (distinct from invalid api key) ---

#[tokio::test]
async fn missing_user_emits_dedicated_user_not_found_code() {
    // A loaded auth principal whose user row has since disappeared must NOT be
    // reported as a bad API key — that confuses ops and clients.
    let store = EntityStore::new();
    let ctx = AuthCtx {
        user_id: UserId::from("ghost"),
        api_key_id: None,
        client_ip: ip(),
    };
    let err = merge_permissions(&ctx, &store, 0, None)
        .await
        .expect_err("missing user must be rejected");
    assert_eq!(err.code, gw_core::error::codes::GW_AUTH_USER_NOT_FOUND);
    assert_ne!(err.code, gw_core::error::codes::GW_AUTH_INVALID_APIKEY);
    assert_eq!(err.status, 401);
}

#[tokio::test]
async fn missing_parent_of_sub_account_emits_user_not_found_code() {
    // The sub-account row exists but its parent (owner) row is absent.
    let store = EntityStore::new();
    store
        .user
        .insert(
            UserId::from("sub"),
            user("sub", Some("missing-owner"), &[], 0, &[], 1.0),
        )
        .await;
    let ctx = AuthCtx {
        user_id: UserId::from("sub"),
        api_key_id: None,
        client_ip: ip(),
    };
    let err = merge_permissions(&ctx, &store, 0, None)
        .await
        .expect_err("sub-account with a missing parent must be rejected");
    assert_eq!(err.code, gw_core::error::codes::GW_AUTH_USER_NOT_FOUND);
    assert_eq!(err.status, 401);
}
