//! UsageRecord schema verification after W4-T1 break.

use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::{ProvenanceTag, UsageSnapshot};
use gw_ledger::usage::{UsageProvenance, UsageRecord, UsageStatus, VendorExposureState};

#[test]
fn schema_serializes_new_fields() {
    let rec = test_record();
    let json = serde_json::to_string(&rec).expect("serialize");
    assert!(
        json.contains("\"attempt_index\":2"),
        "missing attempt_index: {json}"
    );
    assert!(
        json.contains("\"vendor_exposure_state\":\"settled\""),
        "missing exposure state"
    );
    assert!(json.contains("\"usage_provenance\""), "missing provenance");
    assert!(
        json.contains("\"input\":\"upstream_final\""),
        "input provenance label"
    );
    assert!(
        json.contains("\"output\":\"cli_extracted\""),
        "output provenance label"
    );
    assert!(
        !json.contains("\"usage_source\""),
        "old field must be removed"
    );
    assert!(
        !json.contains("\"retry_count\""),
        "old field must be removed"
    );
}

#[test]
fn provenance_tag_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&ProvenanceTag::UpstreamFinal).unwrap(),
        "\"upstream_final\""
    );
    assert_eq!(
        serde_json::to_string(&ProvenanceTag::CliExtracted).unwrap(),
        "\"cli_extracted\""
    );
    assert_eq!(
        serde_json::to_string(&ProvenanceTag::CrashFallback).unwrap(),
        "\"crash_fallback\""
    );
    assert_eq!(
        serde_json::to_string(&ProvenanceTag::Unknown).unwrap(),
        "\"unknown\""
    );
}

#[test]
fn exposure_state_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&VendorExposureState::CrashFallback).unwrap(),
        "\"crash_fallback\""
    );
}

fn test_record() -> UsageRecord {
    UsageRecord {
        ts_ms: 1700000000000,
        request_id: "r1".to_string(),
        trace_id: "t1".to_string(),
        user_id: UserId::from("u1"),
        child_id: None,
        api_key_id: None,
        client_protocol: "openai".to_string(),
        upstream_protocol: "anthropic".to_string(),
        model: ModelName::from("gpt-4"),
        vendor_id: VendorId::from("v1"),
        vendor_key_id: KeyId::from("k1"),
        status: UsageStatus::Ok,
        error_code: None,
        http_status: 200,
        usage: UsageSnapshot {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: Some(20),
            cache_creation_input_tokens: None,
            thinking_tokens: None,
            multimodal_input_tokens: None,
            ..Default::default()
        },
        attempt_index: 2,
        vendor_exposure_state: VendorExposureState::Settled,
        usage_provenance: UsageProvenance {
            input: ProvenanceTag::UpstreamFinal,
            output: ProvenanceTag::CliExtracted,
            cache_read: ProvenanceTag::UpstreamFinal,
            cache_creation: ProvenanceTag::Unknown,
            thinking: ProvenanceTag::Unknown,
            multimodal_input: ProvenanceTag::Unknown,
        },
        freeze_amount_micro: 1500,
        estimate_pricing_version: 0,
        latency_ms: 250,
        ttft_ms: Some(100),
        is_stream: true,
        markup_chain_snapshot: MarkupChainSnapshot::identity(),
        operation_id: None,
        pod_id: String::new(),
        credits_consumed: None,
        coef_table_version: None,
        attempted_keys: Vec::new(),
    }
}
