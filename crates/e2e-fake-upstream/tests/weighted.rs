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

// ── Task 2: chaos flag + dispatch unwrap ─────────────────────────────────────

use e2e_fake_upstream::chaos::set_enabled_for_test;
use e2e_fake_upstream::scenario::{MatchSpec, Route, SseEvent, WeightedVariant};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

fn weighted_route() -> Route {
    Route {
        r#match: MatchSpec {
            method: Some("POST".into()),
            path_regex: Some("^/x$".into()),
            header_eq: None,
        },
        auth_check: None,
        response: ResponseSpec::Weighted {
            variants: vec![
                WeightedVariant {
                    weight: 1,
                    spec: Box::new(ResponseSpec::Sse {
                        delay_ms: 0,
                        events: vec![SseEvent {
                            data: "HAPPY".into(),
                            after_ms: 0,
                        }],
                    }),
                },
                WeightedVariant {
                    weight: 1,
                    spec: Box::new(ResponseSpec::Static {
                        status: 500,
                        headers: Default::default(),
                        body: "FAULT".into(),
                        delay_ms: 0,
                    }),
                },
            ],
        },
    }
}

#[tokio::test(flavor = "current_thread")]
async fn chaos_off_always_picks_first_variant() {
    use e2e_fake_upstream::inject::RefuseCounter;
    use e2e_fake_upstream::routes::make_counters;
    use e2e_fake_upstream::server::serve_business;

    set_enabled_for_test(false);

    let scenario = Scenario {
        routes: vec![weighted_route()],
    };
    let counters = make_counters(&scenario);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let s = Arc::new(RwLock::new(scenario));
    let c = Arc::new(RwLock::new(counters));
    tokio::spawn(serve_business(listener, s, c, RefuseCounter::new()));

    // 5 sequential requests — chaos OFF, variant[0] (HAPPY sse) must win every time.
    for _ in 0..5 {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"POST /x HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let body = String::from_utf8_lossy(&buf);
        assert!(
            body.contains("HAPPY"),
            "chaos=off must serve variant[0]; got: {body}"
        );
        assert!(
            !body.contains("FAULT"),
            "chaos=off leaked variant[1]: {body}"
        );
    }
}
