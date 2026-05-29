//! Audit 2026-05-30 — admit-time status gates: disabled API keys and banned
//! users must be rejected on the business request path. These pin the pure
//! predicate helpers used by `request_filter` (mirrors `expires_at.rs`).
use gw_core::entity::{ApiKeyEntry, ApiKeyStatus, SignatureAlgo, UserEntry};
use gw_core::ids::{ApiKeyId, UserId};
use gw_proxy::filters::request::{check_api_key_status, check_user_status};

fn key_with(status: ApiKeyStatus) -> ApiKeyEntry {
    ApiKeyEntry {
        key_id: ApiKeyId::from("ak_test"),
        user_id: UserId::from("u_test"),
        status,
        hash: vec![0; 32],
        signature_secret: None,
        allowed_models: vec![],
        allowed_channels: vec![],
        rpm_limit: 0,
        tpm_limit: 0,
        markup: 1.0,
        ip_allowlist: None,
        expires_at: None,
        signature_required: false,
        signature_algo: SignatureAlgo::default(),
        force_capabilities: Default::default(),
        match_mode_exclusive: false,
        model_whitelist_regex: None,
    }
}

fn user_with(status: &str) -> UserEntry {
    serde_json::from_value(serde_json::json!({
        "user_id": "u_test",
        "parent_id": null,
        "tier": "free",
        "status": status,
    }))
    .expect("UserEntry fixture deserializes")
}

#[test]
fn active_key_passes() {
    assert!(check_api_key_status(&key_with(ApiKeyStatus::Active)).is_ok());
}

#[test]
fn disabled_key_rejected() {
    assert!(check_api_key_status(&key_with(ApiKeyStatus::Disabled)).is_err());
}

#[test]
fn active_user_passes() {
    assert!(check_user_status(&user_with("active")).is_ok());
}

#[test]
fn banned_user_rejected() {
    assert!(check_user_status(&user_with("banned")).is_err());
}
