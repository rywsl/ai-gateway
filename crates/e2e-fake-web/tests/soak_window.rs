//! Unit tests for the soak window handler payload shapes.
//!
//! These tests stub the Redis surface so we don't need testcontainers in
//! the per-task loop. End-to-end coverage of /soak/window comes via the
//! Python orchestrator under `tests/e2e/client/soak/`.

use e2e_fake_web::soak::{SoakSnapshot, WindowStartReq};

#[test]
fn window_start_req_round_trips() {
    let raw = r#"{"user_ids":["u-1","u-2"]}"#;
    let req: WindowStartReq = serde_json::from_str(raw).unwrap();
    assert_eq!(req.user_ids, vec!["u-1".to_string(), "u-2".to_string()]);
}

#[test]
fn snapshot_serialises_with_required_fields() {
    let snap = SoakSnapshot {
        snapshot_id: "abc123".into(),
        t0_ms: 1_716_412_200_000,
        records_xlen: 0,
        dlq_xlen: 0,
        detailed_xlen: 0,
        events_entity_xlen: 0,
        detailed_last_id: "0-0".into(),
        balance_micros: std::collections::HashMap::from([("u-1".into(), 1_000_000_000_000)]),
    };
    let v = serde_json::to_value(&snap).unwrap();
    assert_eq!(v["snapshot_id"], "abc123");
    assert_eq!(v["t0_ms"], 1_716_412_200_000_i64);
    assert_eq!(v["balance_micros"]["u-1"], 1_000_000_000_000_i64);
}

use e2e_fake_web::soak::quiescence_check;

#[test]
fn quiescence_check_accepts_consecutive_match() {
    let series = vec![100, 105, 110, 110, 110, 110, 110, 110, 110, 110, 110, 110];
    let (quiescent, last) = quiescence_check(&series, 10);
    assert!(quiescent);
    assert_eq!(last, 110);
}

#[test]
fn quiescence_check_rejects_recent_drift() {
    let series = vec![100, 110, 110, 110, 110, 110, 110, 110, 110, 110, 111];
    let (quiescent, _) = quiescence_check(&series, 10);
    assert!(!quiescent);
}

#[test]
fn quiescence_check_empty_or_short_rejects() {
    let (q, _) = quiescence_check(&[], 10);
    assert!(!q);
    let (q, _) = quiescence_check(&[100, 100, 100], 10);
    assert!(!q);
}

use e2e_fake_web::soak::aggregate_spectrum;

#[test]
fn spectrum_buckets_observed_fields_and_marks_absent() {
    let entries: Vec<Vec<(String, String)>> = vec![
        vec![
            ("status".into(), "200".into()),
            ("error_code".into(), "".into()),
            ("protocol".into(), "open_ai".into()),
            ("stream".into(), "true".into()),
            ("protocol_conversion".into(), "false".into()),
        ],
        vec![
            ("status".into(), "429".into()),
            ("error_code".into(), "GW_UPSTREAM_OVERLOADED".into()),
            ("protocol".into(), "anthropic".into()),
            ("stream".into(), "false".into()),
            ("protocol_conversion".into(), "true".into()),
        ],
        vec![
            // missing protocol + stream
            ("status".into(), "502".into()),
            ("error_code".into(), "GW_UPSTREAM_5XX".into()),
        ],
    ];
    let spec = aggregate_spectrum(&entries);
    assert_eq!(spec.by_status.get("200"), Some(&1));
    assert_eq!(spec.by_status.get("429"), Some(&1));
    assert_eq!(spec.by_status.get("502"), Some(&1));
    assert_eq!(spec.by_error_code.get("GW_UPSTREAM_OVERLOADED"), Some(&1));
    assert_eq!(spec.by_error_code.get("__absent__"), Some(&1));
    assert_eq!(spec.by_protocol.get("__absent__"), Some(&1));
    assert_eq!(spec.by_shape.get("stream"), Some(&1));
    assert_eq!(spec.by_shape.get("non_stream"), Some(&1));
    assert_eq!(spec.by_shape.get("__absent__"), Some(&1));
    assert_eq!(spec.by_cross_proto.get("true"), Some(&1));
    assert_eq!(spec.by_cross_proto.get("false"), Some(&1));
}
