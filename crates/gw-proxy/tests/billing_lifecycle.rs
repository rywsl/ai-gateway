use gw_proxy::billing_lifecycle::{
    build_attempt_record, can_transition, is_terminal, on_retry_sync, transition_to,
    VendorExposure,
};
use gw_proxy::ctx::ReqCtx;
use gw_core::entity::VendorKey;
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::{MarkupChainSnapshot, MergedPermissions};
use gw_core::protocol::ApiShape;
use gw_core::usage::ProvenanceTag;
use secrecy::SecretString;
use std::sync::Arc;

#[test]
fn vendor_exposure_default_is_idle() {
    assert!(matches!(VendorExposure::default(), VendorExposure::Idle));
}

#[test]
fn valid_transitions_pass() {
    use VendorExposure::*;
    for (from, to) in [
        (Idle, Sent),
        (Sent, Streaming),
        (Sent, Terminated),
        (Streaming, Settled),
        (Streaming, Terminated),
    ] {
        assert!(
            can_transition(from, to),
            "{from:?} -> {to:?} should be allowed"
        );
    }
}

#[test]
fn invalid_transitions_rejected() {
    use VendorExposure::*;
    for (from, to) in [
        (Idle, Streaming),
        (Idle, Settled),
        (Sent, Settled),
        (Settled, Streaming),
        (Terminated, Settled),
        (Settled, Terminated),
    ] {
        assert!(
            !can_transition(from, to),
            "{from:?} -> {to:?} should be rejected"
        );
    }
}

#[test]
fn terminal_states_identified() {
    use VendorExposure::*;
    assert!(is_terminal(Settled));
    assert!(is_terminal(Terminated));
    assert!(!is_terminal(Idle));
    assert!(!is_terminal(Sent));
    assert!(!is_terminal(Streaming));
}

#[test]
fn transition_to_records_sent_ms() {
    let mut ctx = ReqCtx::default();
    assert!(ctx.timing.sent_ms.is_none());
    transition_to(&mut ctx, VendorExposure::Sent);
    assert!(matches!(ctx.vendor_exposure, VendorExposure::Sent));
    assert!(ctx.timing.sent_ms.is_some());
}

#[test]
fn transition_to_records_settled_ms_for_terminal() {
    let mut ctx = ReqCtx::default();
    transition_to(&mut ctx, VendorExposure::Sent);
    transition_to(&mut ctx, VendorExposure::Streaming);
    assert!(ctx.timing.settled_ms.is_none());
    transition_to(&mut ctx, VendorExposure::Settled);
    assert!(ctx.timing.settled_ms.is_some());
}

#[test]
fn transition_to_records_settled_ms_for_terminated() {
    let mut ctx = ReqCtx::default();
    transition_to(&mut ctx, VendorExposure::Sent);
    transition_to(&mut ctx, VendorExposure::Terminated);
    assert!(ctx.timing.settled_ms.is_some());
}

#[test]
fn transition_to_does_not_overwrite_existing_timestamps() {
    let mut ctx = ReqCtx::default();
    transition_to(&mut ctx, VendorExposure::Sent);
    let original_sent_ms = ctx.timing.sent_ms;
    // Re-entering Sent state should NOT change the timestamp (idempotent)
    // Note: the state machine forbids Sent -> Sent, so we can't directly test
    // this -- but transition_to should not panic on same-state re-entry.
    // (We rely on the debug_assert allowing same-state for idempotency.)
    transition_to(&mut ctx, VendorExposure::Sent);
    assert_eq!(ctx.timing.sent_ms, original_sent_ms);
}

// --- W4-T2: on_retry_sync tests ---

/// Idle path: no upstream contact happened. `on_retry_sync` should just bump
/// `attempt_index` without returning a record (nothing to settle).
#[test]
fn on_retry_sync_idle_bumps_attempt_no_record() {
    let mut ctx = ReqCtx::default();
    assert_eq!(ctx.attempt_index, 0);
    assert!(matches!(ctx.vendor_exposure, VendorExposure::Idle));

    let record = on_retry_sync(&mut ctx);

    assert!(record.is_none(), "Idle path should return no UsageRecord");
    assert_eq!(ctx.attempt_index, 1, "attempt_index must be incremented");
    assert!(
        matches!(ctx.vendor_exposure, VendorExposure::Idle),
        "vendor_exposure stays Idle on no-op path"
    );
}

/// Sent path: upstream was contacted but no response came back.
/// `on_retry_sync` should force Terminated, reset state, and return None
/// because no `picked_vendor_key` is set (default ctx has no vendor key).
#[test]
fn on_retry_sync_sent_without_vendor_key_resets_state() {
    let mut ctx = ReqCtx {
        attempt_index: 0,
        ..ReqCtx::default()
    };
    transition_to(&mut ctx, VendorExposure::Sent);
    // No picked_vendor_key set — build_attempt_record returns None.

    let record = on_retry_sync(&mut ctx);

    assert!(
        record.is_none(),
        "No vendor key means no UsageRecord can be built"
    );
    assert_eq!(ctx.attempt_index, 1, "attempt_index must be incremented");
    assert!(
        matches!(ctx.vendor_exposure, VendorExposure::Idle),
        "vendor_exposure reset to Idle after retry"
    );
    assert!(
        ctx.timing.sent_ms.is_none(),
        "sent_ms cleared after retry reset"
    );
    assert!(
        ctx.cancel_observed_at_ms.is_none(),
        "cancel state cleared after retry reset"
    );
}

/// Verifies that `attempt_index` saturates rather than wrapping on overflow.
#[test]
fn on_retry_sync_attempt_index_saturates() {
    let mut ctx = ReqCtx {
        attempt_index: u32::MAX,
        ..ReqCtx::default()
    };
    // Force Idle so the short-circuit path runs (no vendor key needed).
    let _ = on_retry_sync(&mut ctx);
    assert_eq!(
        ctx.attempt_index,
        u32::MAX,
        "saturating_add should not wrap on overflow"
    );
}

/// Multiple consecutive retries keep incrementing `attempt_index`.
#[test]
fn on_retry_sync_consecutive_retries_increment() {
    let mut ctx = ReqCtx::default();
    assert_eq!(ctx.attempt_index, 0);
    on_retry_sync(&mut ctx); // Idle → 1
    assert_eq!(ctx.attempt_index, 1);
    on_retry_sync(&mut ctx); // Idle → 2
    assert_eq!(ctx.attempt_index, 2);
}

fn seeded_vendor_key() -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from("vk-1"),
        vendor_id: VendorId::from("vendor-1"),
        api_key: SecretString::from("secret".to_string()),
        status: gw_core::entity::KeyStatus::Active,
        accessible_models: vec![ModelName::from("gpt-4o")],
        model_mapping: std::collections::HashMap::new(),
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority: 0,
        weight: 1,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        tags: vec![],
        forced_capabilities: vec![],
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,
        quota_state: Default::default(),
    })
}

fn seeded_cli_ctx() -> ReqCtx {
    let mut ctx = ReqCtx::default();
    ctx.client_shape = Some(ApiShape::OpenAiCli);
    ctx.model = Some(ModelName::from("gpt-4o"));
    ctx.picked_vendor_key = Some(seeded_vendor_key());
    ctx.merged_perm = Some(MergedPermissions {
        user_id: UserId::from("user-1"),
        effective_user_id: UserId::from("user-1"),
        client_ip: "127.0.0.1".parse().unwrap(),
        allowed_models: Default::default(),
        allowed_channels: Default::default(),
        allowed_groups: Default::default(),
        forced_model_group: None,
        rpm_limit: 0,
        tpm_limit: 0,
        max_concurrent_streams: 0,
        markup_chain: MarkupChainSnapshot::identity(),
        model_whitelist_regex: None,
    });
    transition_to(&mut ctx, VendorExposure::Sent);
    transition_to(&mut ctx, VendorExposure::Streaming);
    transition_to(&mut ctx, VendorExposure::Settled);
    ctx
}

#[test]
fn cli_extracted_usage_success_keeps_cli_extracted_provenance() {
    let mut ctx = seeded_cli_ctx();
    ctx.usage.input_tokens = 12;
    ctx.usage_provenance.input = ProvenanceTag::CliExtracted;

    let record = build_attempt_record(&ctx).expect("cli record");

    assert_eq!(record.usage_provenance.input, ProvenanceTag::CliExtracted);
    assert!(record.usage.input_tokens > 0 || record.usage.output_tokens > 0);
    assert_ne!(record.usage_provenance.input, ProvenanceTag::Unknown);
}

#[test]
fn cli_usage_absent_uses_nonzero_estimate_instead_of_free_usage() {
    let mut ctx = seeded_cli_ctx();
    ctx.prompt_estimate_tokens = Some(37);

    let record = build_attempt_record(&ctx).expect("cli record");

    assert_eq!(record.usage.input_tokens, 37);
    assert_eq!(record.usage_provenance.input, ProvenanceTag::Estimated);
    assert!(record.usage.input_tokens > 0 || record.usage.output_tokens > 0);
    assert_ne!(record.usage_provenance.input, ProvenanceTag::Unknown);
}

#[test]
fn cli_usage_absent_without_estimate_fails_billing_protection() {
    let mut ctx = seeded_cli_ctx();
    ctx.prompt_estimate_tokens = None;

    let record = build_attempt_record(&ctx);

    assert!(record.is_none(), "billing protection must fail closed");
}
