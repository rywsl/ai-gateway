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
    let counters = METRICS.iter().filter(|m| matches!(m.kind, MetricKind::Counter)).count();
    let histograms = METRICS.iter().filter(|m| matches!(m.kind, MetricKind::Histogram)).count();
    let gauges = METRICS.iter().filter(|m| matches!(m.kind, MetricKind::Gauge)).count();
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
