//! Unit tests for the `ResponseSpec::Weighted` DSL variant (E2E chaos soak).

use e2e_fake_upstream::scenario::{validate_scenario, ResponseSpec, Scenario};

const HAPPY_WEIGHTED_JSON: &str = r#"{
    "routes": [{
        "match": {"method": "POST", "path_regex": "^/v1/chat/completions$"},
        "response": {
            "kind": "weighted",
            "variants": [
                {"weight": 90, "spec": {"kind": "sse", "delay_ms": 0, "events": [{"data": "hi"}]}},
                {"weight": 10, "spec": {"kind": "tcp_reset_mid_body", "after_bytes": 256}}
            ]
        }
    }]
}"#;

#[test]
fn weighted_variant_round_trips() {
    let s: Scenario = serde_json::from_str(HAPPY_WEIGHTED_JSON).unwrap();
    assert_eq!(s.routes.len(), 1);
    match &s.routes[0].response {
        ResponseSpec::Weighted { variants } => {
            assert_eq!(variants.len(), 2);
            assert_eq!(variants[0].weight, 90);
            assert_eq!(variants[1].weight, 10);
        }
        other => panic!("expected Weighted, got {other:?}"),
    }
    // Positive case: a well-formed Weighted scenario must pass validation.
    validate_scenario(&s).expect("happy-path Weighted scenario must validate");
}

#[test]
fn validate_rejects_empty_variants() {
    let raw = r#"{
        "routes": [{
            "match": {"method": "POST", "path_regex": "^/x$"},
            "response": {"kind": "weighted", "variants": []}
        }]
    }"#;
    let s: Scenario = serde_json::from_str(raw).unwrap();
    let err = validate_scenario(&s).expect_err("empty variants must be rejected");
    assert!(err.contains("empty variants"), "got: {err}");
}

#[test]
fn validate_rejects_nested_weighted() {
    let raw = r#"{
        "routes": [{
            "match": {"method": "POST", "path_regex": "^/x$"},
            "response": {
                "kind": "weighted",
                "variants": [
                    {"weight": 1, "spec": {
                        "kind": "weighted",
                        "variants": [
                            {"weight": 1, "spec": {"kind": "sse", "delay_ms": 0, "events": []}}
                        ]
                    }}
                ]
            }
        }]
    }"#;
    let s: Scenario = serde_json::from_str(raw).unwrap();
    let err = validate_scenario(&s).expect_err("nested weighted must be rejected");
    assert!(err.contains("nested"), "got: {err}");
}
