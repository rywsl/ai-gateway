use std::sync::Arc;

use gw_core::error::message_rules::{ErrorMessageMatcher, ErrorMessageRule};
use gw_store::error_message_rules::ErrorMessageRuleStore;
use gw_store::stream_consumer::apply_error_message_rules_event_for_test;

#[tokio::test]
async fn bad_snapshot_keeps_old_compiled_rules() {
    let store = ErrorMessageRuleStore::empty_with_rules(vec![valid_rule("old")]).unwrap();
    assert_eq!(store.snapshot().rule_count(), 1);

    let err = store
        .replace_for_test(vec![invalid_rule_with_too_many_values()])
        .await
        .unwrap_err();

    assert!(err.to_string().contains("too many values"));
    assert_eq!(store.snapshot().rule_count(), 1);
}

#[tokio::test]
async fn stream_event_for_error_message_rules_is_consumed() {
    let consumed =
        apply_error_message_rules_event_for_test(None, "error_message_rules", "global").await;
    assert!(consumed);

    let ignored = apply_error_message_rules_event_for_test(None, "model", "gpt-test").await;
    assert!(!ignored);
}

#[tokio::test]
async fn stream_event_triggers_reload_without_replacing_on_bad_snapshot() {
    let store = Arc::new(ErrorMessageRuleStore::empty_for_test());
    let consumed = apply_error_message_rules_event_for_test(
        Some(store.clone()),
        "error_message_rules",
        "global",
    )
    .await;
    assert!(consumed);

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(store.snapshot().rule_count(), 0);
}

fn valid_rule(id: &str) -> ErrorMessageRule {
    ErrorMessageRule {
        id: id.to_string(),
        enabled: true,
        priority: 10,
        matcher: ErrorMessageMatcher {
            models: vec!["gpt-image-2-max".to_string()],
            upstream_message_contains: vec!["unsupported".to_string()],
            ..Default::default()
        },
        message: "use image endpoint".to_string(),
    }
}

fn invalid_rule_with_too_many_values() -> ErrorMessageRule {
    let mut rule = valid_rule("bad");
    rule.matcher.models = (0..51).map(|idx| format!("model-{idx}")).collect();
    rule
}
