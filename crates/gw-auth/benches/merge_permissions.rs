//! `gw_auth::merge_permissions` microbenchmark (AC-023-5).
//!
//! Builds an `EntityStore` in-memory with one main-account user belonging to
//! 10 user-groups + one api-key (all tiers populated), then times the full
//! five-tier merge. The store is populated once outside the timed loop;
//! the bench thread then drives `merge_permissions` via a fresh single-threaded
//! tokio runtime per call so we measure the `.await` chain end-to-end without
//! contention.
//!
//! Spec target (record gap as TODO if missed):
//!   P99 ≤ 5 µs at 10 user_group scenario.
//!
//! Run with: `cargo bench -p gw-auth --bench merge_permissions`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gw_auth::{merge_permissions, AuthCtx};
use gw_core::entity::*;
use gw_core::ids::*;
use gw_core::versioned::Versioned;
use gw_store::EntityStore;
use std::net::IpAddr;
use std::sync::Arc;

fn make_user(id: &str, groups: &[&str]) -> Arc<Versioned<UserEntry>> {
    Arc::new(Versioned::new(
        1,
        UserEntry {
            user_id: UserId::from(id),
            parent_id: None,
            tier: "standard".into(),
            status: UserStatus::Active,
            token_version: 0,
            allowed_models: vec![
                ModelName::from("gpt-4"),
                ModelName::from("gpt-3.5"),
                ModelName::from("claude-3-5-sonnet"),
                ModelName::from("gemini-1.5-pro"),
            ],
            allowed_channels: vec![],
            allowed_groups: vec![],
            rpm_limit: 1000,
            tpm_limit: 0,
            max_concurrent_streams: 100,
            daily_quota_tokens: 0,
            user_group_ids: groups.iter().copied().map(UserGroupId::from).collect(),
            markup: 1.0,
            debug_enabled: false,
        },
    ))
}

fn make_group(id: &str) -> Arc<Versioned<UserGroup>> {
    Arc::new(Versioned::new(
        1,
        UserGroup {
            id: UserGroupId::from(id),
            display_name: id.to_string(),
            priority: 0,
            allowed_models: vec![
                ModelName::from("gpt-4"),
                ModelName::from("claude-3-5-sonnet"),
            ],
            allowed_channels: vec![],
            model_group_binding: None,
            rpm_limit: 500,
            tpm_limit: 0,
            max_concurrent_streams: 0,
            markup: 1.0,
        },
    ))
}

fn make_apikey(id: &str) -> Arc<Versioned<ApiKeyEntry>> {
    Arc::new(Versioned::new(
        1,
        ApiKeyEntry {
            key_id: ApiKeyId::from(id),
            user_id: UserId::from("u1"),
            status: ApiKeyStatus::Active,
            hash: vec![0; 32],
            signature_secret: None,
            allowed_models: vec![ModelName::from("gpt-4")],
            allowed_channels: vec![],
            rpm_limit: 200,
            tpm_limit: 0,
            markup: 1.0,
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

fn seed_store_with_10_groups() -> (EntityStore, AuthCtx) {
    // Use a private runtime to drive the async insert calls; we throw it
    // away before the bench harness starts timing.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build init runtime");
    let store = rt.block_on(async {
        let store = EntityStore::new();
        let group_ids: Vec<String> = (0..10).map(|i| format!("g{i}")).collect();
        let group_id_refs: Vec<&str> = group_ids.iter().map(String::as_str).collect();
        store
            .user
            .insert(UserId::from("u1"), make_user("u1", &group_id_refs))
            .await;
        for gid in &group_ids {
            store
                .user_group
                .insert(UserGroupId::from(gid.as_str()), make_group(gid))
                .await;
        }
        store
            .apikey
            .insert(ApiKeyId::from("ak1"), make_apikey("ak1"))
            .await;
        store
    });
    let ctx = AuthCtx {
        user_id: UserId::from("u1"),
        api_key_id: Some(ApiKeyId::from("ak1")),
        client_ip: "127.0.0.1".parse::<IpAddr>().unwrap(),
    };
    (store, ctx)
}

fn bench_merge(c: &mut Criterion) {
    let (store, ctx) = seed_store_with_10_groups();
    // A long-lived runtime drives the .await chain. `block_on` adds some
    // overhead but is the standard criterion + async pattern; the relative
    // cost between revisions remains comparable.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build bench runtime");

    c.bench_function("merge_permissions_10_groups_with_apikey", |b| {
        b.iter(|| {
            let perm = rt.block_on(async {
                merge_permissions(black_box(&ctx), black_box(&store), 100_000, None)
                    .await
                    .expect("merge")
            });
            black_box(perm);
        });
    });
}

criterion_group!(benches, bench_merge);
criterion_main!(benches);
