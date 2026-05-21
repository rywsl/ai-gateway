//! G1 — ApiKeyEntry.expires_at enforcement.
use gw_core::entity::{ApiKeyEntry, ApiKeyStatus, SignatureAlgo};
use gw_core::ids::{ApiKeyId, UserId};
use gw_proxy::filters::request::check_api_key_expiry;
use time::OffsetDateTime;

fn entry_with(exp: Option<OffsetDateTime>) -> ApiKeyEntry {
    ApiKeyEntry {
        key_id: ApiKeyId::from("ak_test"),
        user_id: UserId::from("u_test"),
        status: ApiKeyStatus::Active,
        hash: vec![0; 32],
        allowed_models: vec![],
        allowed_channels: vec![],
        rpm_limit: 0,
        tpm_limit: 0,
        markup: 1.0,
        ip_allowlist: None,
        expires_at: exp,
        signature_required: false,
        signature_algo: SignatureAlgo::default(),
        force_capabilities: Default::default(),
        match_mode_exclusive: false,
    }
}

#[test]
fn none_means_unlimited() {
    assert!(check_api_key_expiry(&entry_with(None), OffsetDateTime::now_utc()).is_ok());
}

#[test]
fn future_passes() {
    let now = OffsetDateTime::now_utc();
    let exp = now + time::Duration::hours(1);
    assert!(check_api_key_expiry(&entry_with(Some(exp)), now).is_ok());
}

#[test]
fn past_rejects() {
    let now = OffsetDateTime::now_utc();
    let exp = now - time::Duration::seconds(1);
    assert!(check_api_key_expiry(&entry_with(Some(exp)), now).is_err());
}

#[test]
fn exact_moment_rejects() {
    let now = OffsetDateTime::now_utc();
    assert!(check_api_key_expiry(&entry_with(Some(now)), now).is_err());
}
