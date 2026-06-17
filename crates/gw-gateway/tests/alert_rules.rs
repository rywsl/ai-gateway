//! D19/D20 (#17): static verification that the page-on-call billing/credit
//! alert rules exist and reference every P1 counter that the Rust catalog
//! zero-initializes. This keeps the alert file and the
//! `PAGE_ON_CALL_BILLING_COUNTERS` set from drifting apart without needing a
//! running Prometheus.

use gw_gateway::metrics_catalog::PAGE_ON_CALL_BILLING_COUNTERS;
use std::path::PathBuf;

fn billing_rules_yaml() -> String {
    // crates/gw-gateway/Cargo.toml -> repo root is two levels up.
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/prometheus/billing-rules.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[test]
fn billing_rules_file_exists_and_nonempty() {
    let yaml = billing_rules_yaml();
    assert!(yaml.contains("groups:"), "billing-rules.yml missing groups");
    assert!(
        yaml.contains("billing-page-on-call"),
        "billing-rules.yml missing the page-on-call group"
    );
}

/// Every zero-initialized page-on-call counter must have an alert whose `expr`
/// references it. The set is the single source of truth (catalog side); this
/// asserts the YAML side covers all of them.
#[test]
fn every_page_on_call_counter_has_an_alert() {
    let yaml = billing_rules_yaml();
    for counter in PAGE_ON_CALL_BILLING_COUNTERS {
        assert!(
            yaml.contains(counter),
            "billing-rules.yml has no alert referencing page-on-call counter `{counter}`"
        );
    }
}

/// Each of the five #17 D19/D20 counters must be present, and each should be
/// fired off a non-zero `increase(...)` expression so the boot zero-baseline
/// reads healthy.
#[test]
fn five_p1_counters_alerted_on_increase() {
    let yaml = billing_rules_yaml();
    for counter in [
        "gw_billing_emit_lag_t0_total",
        "gw_billing_flusher_panic_total",
        "gw_billing_flusher_dead_total",
        "gw_credit_lease_expired_total",
        "gw_billing_inflight_register_failure_total",
    ] {
        let needle = format!("increase({counter}[5m])");
        assert!(
            yaml.contains(&needle),
            "billing-rules.yml missing `{needle}` alert expression"
        );
    }
}
