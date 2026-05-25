use gw_gateway::metrics_catalog::{describe_all, MetricKind, METRICS};

#[test]
fn catalog_includes_required_families() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    for required in [
        "gw_codec_convert_total",
        "gw_codec_convert_duration_seconds",
        "gw_codec_passthrough_bytes_total",
        "gw_freeze_acquire_total",
        "gw_freeze_release_total",
        "gw_freeze_estimate_micro",
        "gw_auth_failure_total",
        "gw_target_filter_denied_total",
        "gw_redos_timeout_total",
        "gw_kms_state",
        "gw_kms_reencrypt_progress",
        "gw_oauth_refresh_total",
        "gw_oauth_token_age_seconds",
        "gw_capability_resolve_total",
        "gw_capability_unknown_total",
        "gw_resilience_redis_error_total",
        "gw_resilience_freeze_fallback_total",
        "gw_resilience_cooldown_fallback_total",
        "gw_detailed_log_channel_depth",
        "gw_detailed_log_compress_seconds",
        "gw_detailed_log_drop_total",
        "gw_detailed_log_emit_total",
        "gw_detailed_log_payload_bytes",
        "gw_detailed_log_pre_compress_bytes",
    ] {
        assert!(names.contains(&required), "missing metric: {required}");
    }
}

#[test]
fn catalog_kind_distribution_sane() {
    let counters = METRICS
        .iter()
        .filter(|m| matches!(m.kind, MetricKind::Counter))
        .count();
    let histograms = METRICS
        .iter()
        .filter(|m| matches!(m.kind, MetricKind::Histogram))
        .count();
    let gauges = METRICS
        .iter()
        .filter(|m| matches!(m.kind, MetricKind::Gauge))
        .count();
    assert!(counters >= 10);
    assert!(histograms >= 2);
    assert!(gauges >= 2);
}

#[test]
fn describe_all_runs_clean() {
    // The metrics-exporter-prometheus may not be installed in test mode; describe_*
    // macros should still no-op gracefully.
    describe_all();
}

#[test]
fn no_duplicate_metric_names() {
    let mut names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    names.sort();
    let before = names.len();
    names.dedup();
    assert_eq!(names.len(), before, "duplicate metric name detected");
}

#[test]
fn billing_w1_metrics_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    assert!(
        names.contains(&"gw_billing_emit_total"),
        "missing metric: gw_billing_emit_total"
    );
    assert!(
        names.contains(&"gw_billing_provenance_total"),
        "missing metric: gw_billing_provenance_total"
    );
}

#[test]
fn billing_w2_metrics_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    assert!(names.contains(&"gw_billing_instance_heartbeat_renewals_total"));
}

#[test]
fn billing_w2_t4_exposure_transitions_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    assert!(names.contains(&"gw_billing_exposure_transitions_total"));
}

#[test]
fn billing_w2_t6_inflight_register_failure_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    assert!(
        names.contains(&"gw_billing_inflight_register_failure_total"),
        "missing metric: gw_billing_inflight_register_failure_total"
    );
}

#[test]
fn billing_w2_t7_must_deliver_metrics_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    assert!(
        names.contains(&"gw_billing_mpsc_bypass_total"),
        "missing metric: gw_billing_mpsc_bypass_total"
    );
    assert!(
        names.contains(&"gw_billing_emit_path_total"),
        "missing metric: gw_billing_emit_path_total"
    );
}

#[test]
fn billing_w3_t1_upstream_cancellation_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    assert!(names.contains(&"gw_billing_upstream_cancellation_total"));
}

#[test]
fn billing_w3_t5_grace_metric_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    assert!(names.contains(&"gw_billing_grace_yielded_final_usage_total"));
}

#[test]
fn billing_w4_t3_emit_lag_t0_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    assert!(names.contains(&"gw_billing_emit_lag_t0_total"));
}

#[test]
fn billing_w4_t4_inflight_recovered_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    assert!(names.contains(&"gw_billing_inflight_recovered_total"));
}

#[test]
fn runtime_operational_metrics_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    for required in [
        "gateway_started_total",
        "gateway_requests_total",
        "gateway_tpm_consumed_total",
        "gateway_freeze_leak_recovered_total",
        "gateway_usage_records_lost_total",
        "gateway_usage_channel_full_total",
        "gateway_usage_serialize_errors_total",
        "gateway_usage_queue_errors_total",
        "gateway_usage_flush_errors_total",
        "gw_deduct_gc_removed_total",
        "gw_forced_params_applied_total",
        "gw_classifier_outcome_total",
        "gw_error_handler_ledger_total",
        "gw_daily_quota_increment_error_total",
    ] {
        assert!(names.contains(&required), "missing metric: {required}");
    }
}
