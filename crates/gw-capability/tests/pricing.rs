use gw_capability::pricing::{InMemoryPricingReader, PricingSnapshot, PricingSnapshotReader};

#[tokio::test]
async fn in_memory_reader_round_trip() {
    let r = InMemoryPricingReader::default();
    let s = PricingSnapshot {
        base_input_micro_per_token: 30,
        base_output_micro_per_token: 150,
        last_updated_ms: 1,
    };
    r.insert("claude-3-opus", "anthropic", s);
    let got = r.get("claude-3-opus", "anthropic").await.unwrap();
    assert_eq!(got.base_output_micro_per_token, 150);
    assert_eq!(got.base_input_micro_per_token, 30);
    assert_eq!(got.last_updated_ms, 1);
}

#[tokio::test]
async fn missing_returns_none() {
    let r = InMemoryPricingReader::default();
    let got = r.get("none", "none").await;
    assert!(got.is_none());
}

#[tokio::test]
async fn multiple_vendors_isolated() {
    let r = InMemoryPricingReader::default();
    r.insert(
        "model-x",
        "vendor-a",
        PricingSnapshot {
            base_input_micro_per_token: 10,
            base_output_micro_per_token: 20,
            last_updated_ms: 1,
        },
    );
    r.insert(
        "model-x",
        "vendor-b",
        PricingSnapshot {
            base_input_micro_per_token: 30,
            base_output_micro_per_token: 40,
            last_updated_ms: 2,
        },
    );
    assert_eq!(
        r.get("model-x", "vendor-a")
            .await
            .unwrap()
            .base_input_micro_per_token,
        10
    );
    assert_eq!(
        r.get("model-x", "vendor-b")
            .await
            .unwrap()
            .base_input_micro_per_token,
        30
    );
}
