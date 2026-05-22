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
