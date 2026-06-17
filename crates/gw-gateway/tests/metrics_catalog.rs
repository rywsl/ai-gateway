use gw_gateway::metrics_catalog::{
    describe_all, zero_init_page_on_call, MetricKind, MetricSpec, METRICS,
    PAGE_ON_CALL_BILLING_COUNTERS,
};

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
fn stream_probe_metrics_registered_without_key_level_labels() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    for required in [
        "gw_stream_probe_total",
        "gw_stream_probe_failover_total",
        "gw_stream_probe_buffer_bytes",
        "gw_stream_probe_timeout_total",
    ] {
        assert!(names.contains(&required), "missing metric: {required}");
    }

    for metric in METRICS
        .iter()
        .filter(|m| m.name.starts_with("gw_stream_probe_"))
    {
        assert!(!metric.help.contains("key_id"));
        assert!(!metric.help.contains("vendor_key"));
        assert!(!metric.help.contains("api_key"));
        assert!(!metric.help.contains("user_id"));
    }
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

// --- P0 #30: page-on-call billing metrics must be visible in /metrics at boot ---

/// Helper: render a fresh, isolated Prometheus recorder after running `setup`
/// against it. Uses `with_local_recorder` so no global recorder is installed
/// and tests stay independent of each other / of process-wide state.
fn render_with<F: FnOnce()>(setup: F) -> String {
    use metrics_exporter_prometheus::PrometheusBuilder;
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::with_local_recorder(&recorder, setup);
    handle.render()
}

/// Diagnostic / root-cause lock-in: `describe_all()` alone (HELP/TYPE only)
/// does NOT create the series, so the page-on-call counters are ABSENT from a
/// rendered `/metrics`. This is exactly why a zero-init boot step is required.
#[test]
fn describe_only_does_not_render_page_on_call_counters() {
    let rendered = render_with(|| {
        describe_all();
    });
    for name in PAGE_ON_CALL_BILLING_COUNTERS {
        assert!(
            !rendered.contains(name),
            "expected describe-only render to OMIT {name}; if this now renders, \
             the exporter behaviour changed and zero-init may be redundant"
        );
    }
}

/// Fix verification: after the boot sequence (`describe_all` + zero-init), each
/// page-on-call counter is present in `/metrics` with a `0` baseline, before any
/// incident has occurred — so the Go-side alert rules evaluate against a healthy
/// 0 rather than an absent series.
#[test]
fn boot_init_renders_page_on_call_counters_at_zero() {
    let rendered = render_with(|| {
        describe_all();
        zero_init_page_on_call();
    });
    for name in PAGE_ON_CALL_BILLING_COUNTERS {
        assert!(
            rendered.contains(name),
            "metric {name} absent from /metrics after boot zero-init:\n{rendered}"
        );
        // Counter renders as `<name> 0` (no labels) at the zero baseline.
        assert!(
            rendered.contains(&format!("{name} 0")),
            "metric {name} should render with a 0 baseline:\n{rendered}"
        );
    }
}

/// Consistency: every page-on-call name exists in the static catalog and is
/// documented as page-on-call, so the boot zero-init set can't drift from the
/// catalog or silently lose its severity contract.
#[test]
fn page_on_call_set_is_subset_of_catalog() {
    for name in PAGE_ON_CALL_BILLING_COUNTERS {
        let spec: &MetricSpec = METRICS
            .iter()
            .find(|m| m.name == *name)
            .unwrap_or_else(|| panic!("page-on-call metric {name} missing from catalog"));
        assert!(
            matches!(spec.kind, MetricKind::Counter),
            "page-on-call metric {name} must be a Counter"
        );
        assert!(
            spec.help.contains("page on-call"),
            "page-on-call metric {name} help must document the page-on-call contract"
        );
    }
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
        "gw_usage_dlq_emergency_file_total",
        "gw_usage_dlq_retry_queue_total",
        "gw_frozen_delta_enqueue_total",
        "gw_frozen_delta_redis_total",
        "gw_deduct_gc_removed_total",
        "gw_forced_params_applied_total",
        "gw_classifier_outcome_total",
        "gw_error_handler_ledger_total",
        "gw_daily_quota_increment_error_total",
    ] {
        assert!(names.contains(&required), "missing metric: {required}");
    }
}

/// D3/D4 (#17): no catalog metric may advertise an unbounded per-identity label
/// (`user_id` / `api_key_id`). These were removed from `gateway_tpm_consumed_total`
/// and `gw_redos_timeout_total`; this test locks the cardinality contract so a
/// future edit can't silently reintroduce a label-cardinality explosion. The
/// bounded tunnel `node_id` label is intentionally allowed (see D5) and is not
/// covered by this check.
#[test]
fn no_high_cardinality_id_labels_in_help() {
    for m in METRICS {
        for banned in ["user_id", "api_key_id"] {
            assert!(
                !m.help.contains(banned),
                "metric {} help advertises high-cardinality label `{banned}`: {:?}",
                m.name,
                m.help
            );
        }
    }
}

/// D9 (#17): the credit-lease observability gauges are part of the catalog and
/// their HELP no longer carries the "sampler not yet wired" TODO. The sampler is
/// in `gw_ledger::ledger_sweep`.
#[test]
fn credit_lease_gauges_registered() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    for required in [
        "gw_credit_lease_active_users",
        "gw_credit_coef_table_version_active",
    ] {
        assert!(names.contains(&required), "missing metric: {required}");
    }
    for m in METRICS.iter().filter(|m| {
        m.name == "gw_credit_lease_active_users" || m.name == "gw_credit_coef_table_version_active"
    }) {
        assert!(
            !m.help.to_ascii_lowercase().contains("todo"),
            "{} help should not advertise a TODO once the sampler is wired",
            m.name
        );
    }
}
