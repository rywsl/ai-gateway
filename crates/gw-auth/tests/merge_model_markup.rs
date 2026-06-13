//! G5 (T2-B) — `Pricing.model_markup` must land in
//! `MarkupChainSnapshot.model_markup` instead of the legacy 1.0 placeholder.
//!
//! ADR ref: docs/ROADMAP.md.
//!
//! Rust still does not compute cost (`no_cost_micro` invariant); the chain is
//! emitted verbatim into `UsageRecord.markup_chain_snapshot` and Go multiplies.

use gw_auth::{merge_permissions, AuthCtx};
use gw_core::entity::*;
use gw_core::ids::*;
use gw_core::versioned::Versioned;
use gw_store::EntityStore;
use std::net::IpAddr;
use std::sync::Arc;

fn user_no_markup() -> Arc<Versioned<UserEntry>> {
    Arc::new(Versioned::new(
        1,
        UserEntry {
            user_id: UserId::from("u1"),
            parent_id: None,
            tier: "standard".into(),
            status: UserStatus::Active,
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
async fn pricing_model_markup_lands_in_chain() {
    let store = EntityStore::new();
    store
        .user
        .insert(UserId::from("u1"), user_no_markup())
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: None,
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 0, Some(pricing("gpt-4", 1.25)))
        .await
        .expect("merge");
    assert!(
        (perm.markup_chain.model_markup - 1.25).abs() < 1e-4,
        "expected model_markup=1.25, got {}",
        perm.markup_chain.model_markup
    );
    assert!(
        (perm.markup_chain.product - 1.25).abs() < 1e-4,
        "expected product to include model_markup, got {}",
        perm.markup_chain.product
    );
}

#[tokio::test]
async fn no_pricing_defaults_to_one() {
    let store = EntityStore::new();
    store
        .user
        .insert(UserId::from("u1"), user_no_markup())
        .await;

    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: None,
        client_ip: ip(),
    };
    let perm = merge_permissions(&ctx, &store, 0, None)
        .await
        .expect("merge");
    assert!(
        (perm.markup_chain.model_markup - 1.0).abs() < 1e-4,
        "expected model_markup=1.0 placeholder when pricing is None, got {}",
        perm.markup_chain.model_markup
    );
    assert!(
        (perm.markup_chain.product - 1.0).abs() < 1e-4,
        "expected product=1.0 when pricing is None, got {}",
        perm.markup_chain.product
    );
}
