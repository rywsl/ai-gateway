from soak.driver import SoakDriverReport
from soak.invariants import (
    process_alive_no_panic,
    process_alive_livez_throughout,
    process_alive_readyz_throughout,
    process_alive_no_container_restart,
)


def test_no_panic_pass_on_clean_logs():
    leaf = process_alive_no_panic("2026-05-22 INFO gateway: started")
    assert leaf["passed"] is True
    assert leaf["raw"]["match_count"] == 0
    assert leaf["raw"]["matched_lines"] == []
    assert leaf["kind"] == "hard"
    assert leaf["source"] == "docker.compose_logs.gateway"


def test_no_panic_fail_on_panic_line():
    leaf = process_alive_no_panic(
        "INFO gateway: started\nthread 'tokio' panicked at boom"
    )
    assert leaf["passed"] is False
    assert leaf["raw"]["match_count"] == 1


def test_livez_throughout_pass():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.livez_probes = 6
    rpt.livez_non_200 = 0
    leaf = process_alive_livez_throughout(rpt)
    assert leaf["passed"] is True
    assert leaf["raw"]["non_200_count"] == 0


def test_livez_throughout_fail():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.livez_probes = 6
    rpt.livez_non_200 = 1
    leaf = process_alive_livez_throughout(rpt)
    assert leaf["passed"] is False


def test_no_container_restart_pass():
    leaf = process_alive_no_container_restart(
        before={"RestartCount": 0, "State": {"Status": "running"}},
        after={"RestartCount": 0, "State": {"Status": "running"}},
    )
    assert leaf["passed"] is True


def test_no_container_restart_fail_on_restart_increment():
    leaf = process_alive_no_container_restart(
        before={"RestartCount": 0, "State": {"Status": "running"}},
        after={"RestartCount": 1, "State": {"Status": "running"}},
    )
    assert leaf["passed"] is False
    assert leaf["raw"]["restart_count_delta"] == 1


from soak.invariants import (
    account_records_equals_upstream_hits,
    account_balance_monotonic_decreasing,
    account_drain_quiescence_reached,
)


def test_records_equals_hits_pass():
    leaf = account_records_equals_upstream_hits(
        window={
            "redis_streams": {
                "usage_records_delta": 50001,
                "usage_records_dlq_delta": 0,
            },
            "drain_observation_series": [49991, 50001, 50001, 50001, 50001,
                                          50001, 50001, 50001, 50001, 50001],
        },
        hits={"routes_hit_sum": 50001, "refused_hit_sum": 0, "total": 50001},
    )
    assert leaf["passed"] is True
    assert leaf["raw"]["diff"] == 0
    assert leaf["threshold"] == "=="


def test_records_equals_hits_fail_diff_1():
    leaf = account_records_equals_upstream_hits(
        window={
            "redis_streams": {
                "usage_records_delta": 49997,
                "usage_records_dlq_delta": 3,
            },
            "drain_observation_series": [49997, 50000, 50000, 50000, 50000,
                                          50000, 50000, 50000, 50000, 50000],
        },
        hits={"routes_hit_sum": 49998, "refused_hit_sum": 3, "total": 50001},
    )
    assert leaf["passed"] is False
    assert leaf["raw"]["diff"] == 1


def test_balance_monotonic_pass():
    leaf = account_balance_monotonic_decreasing(
        window={"balance_decrement_micros": {"u-1": 1000, "u-2": 0, "u-3": 50}},
    )
    assert leaf["passed"] is True


def test_balance_monotonic_fail_on_negative_decrement():
    leaf = account_balance_monotonic_decreasing(
        window={"balance_decrement_micros": {"u-1": 1000, "u-2": -50}},
    )
    assert leaf["passed"] is False
    assert leaf["raw"]["negative_users"] == ["u-2"]


def test_drain_quiescence_pass():
    leaf = account_drain_quiescence_reached(window={"drain_quiescent": True})
    assert leaf["passed"] is True


def test_drain_quiescence_fail():
    leaf = account_drain_quiescence_reached(window={"drain_quiescent": False})
    assert leaf["passed"] is False


from soak.invariants import (
    resource_all_in_flight_gauges_zero,
    resource_rss_growth_under_threshold,
    error_code_compliance_all_in_catalog,
    chaos_causal_per_variant,
    min_completion_at_least_half,
    VARIANT_ERROR_CODE_MAP,
)


def test_all_in_flight_gauges_zero_pass():
    metrics_text = """\
gw_routing_inflight_per_key{key_id="vk-1"} 0
gw_proxy_streams_in_flight 0
"""
    leaf = resource_all_in_flight_gauges_zero(metrics_text)
    assert leaf["passed"] is True
    assert leaf["raw"]["non_zero_gauges"] == {}


def test_all_in_flight_gauges_zero_fail():
    metrics_text = """\
gw_routing_inflight_per_key{key_id="vk-1"} 2
gw_proxy_streams_in_flight 0
"""
    leaf = resource_all_in_flight_gauges_zero(metrics_text)
    assert leaf["passed"] is False
    assert leaf["raw"]["non_zero_gauges"][
        'gw_routing_inflight_per_key{key_id="vk-1"}'
    ] == 2.0


def test_rss_growth_pass():
    leaf = resource_rss_growth_under_threshold(
        before_bytes=100 * 1024 * 1024,
        after_bytes=180 * 1024 * 1024,
        threshold_mib=100,
    )
    assert leaf["passed"] is True


def test_rss_growth_fail():
    leaf = resource_rss_growth_under_threshold(
        before_bytes=100 * 1024 * 1024,
        after_bytes=300 * 1024 * 1024,
        threshold_mib=100,
    )
    assert leaf["passed"] is False


def test_error_code_compliance_pass():
    leaf = error_code_compliance_all_in_catalog(
        observed={"GW_UPSTREAM_TIMEOUT", "GW_UPSTREAM_5XX"},
        catalog={"GW_UPSTREAM_TIMEOUT", "GW_UPSTREAM_5XX", "GW_AUTH_INVALID_APIKEY"},
        catalog_source="codes.rs@abc1234",
    )
    assert leaf["passed"] is True
    assert leaf["raw"]["unknown"] == []


def test_error_code_compliance_fail():
    leaf = error_code_compliance_all_in_catalog(
        observed={"GW_UPSTREAM_TIMEOUT", "GW_MADE_UP_CODE"},
        catalog={"GW_UPSTREAM_TIMEOUT"},
        catalog_source="codes.rs@abc1234",
    )
    assert leaf["passed"] is False
    assert "GW_MADE_UP_CODE" in leaf["raw"]["unknown"]


def test_variant_error_code_map_codes_exist_in_typical_catalog():
    typical = {
        "GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_CANCELLED",
        "GW_UPSTREAM_TIMEOUT", "GW_UPSTREAM_OVERLOADED", "GW_UPSTREAM_5XX",
        "HTTP_STATUS_429", "HTTP_STATUS_502", "CLIENT_SIDE_FAILURE",
    }
    for variant_kind, codes in VARIANT_ERROR_CODE_MAP.items():
        assert codes, f"variant {variant_kind} has empty code set"
        for c in codes:
            assert c in typical, f"{variant_kind}: {c} not in typical catalog"


def test_chaos_causal_pass():
    chaos_stats = {
        "enabled": True,
        "variants": [
            {"variant_index": 0, "kind": "sse", "hits": 100, "weight": 80},
            {"variant_index": 1, "kind": "tcp_reset_mid_body",
             "hits": 10, "weight": 5},
        ],
    }
    by_error_code = {"GW_UPSTREAM_CONNECT_FAILED": 9, "GW_UPSTREAM_CANCELLED": 0}
    leaf = chaos_causal_per_variant(chaos_stats, by_error_code, min_ratio=0.7)
    per = leaf["raw"]["per_variant"]
    assert per["tcp_reset_mid_body"]["passed"] is True
    assert per["tcp_reset_mid_body"]["matched_error_count"] == 9
    assert leaf["passed"] is True


def test_chaos_causal_accepts_http_status_surrogates():
    chaos_stats = {
        "enabled": True,
        "variants": [
            {"variant_index": 2, "kind": "status_with_retry_after",
             "hits": 10, "weight": 5},
            {"variant_index": 6, "kind": "static", "hits": 10, "weight": 2},
        ],
    }
    by_error_code = {"HTTP_STATUS_429": 8, "HTTP_STATUS_502": 8}
    leaf = chaos_causal_per_variant(chaos_stats, by_error_code, min_ratio=0.7)
    assert leaf["raw"]["per_variant"]["status_with_retry_after"]["passed"] is True
    assert leaf["raw"]["per_variant"]["static"]["passed"] is True
    assert leaf["passed"] is True


def test_chaos_causal_fail_when_no_mapped_codes():
    chaos_stats = {
        "enabled": True,
        "variants": [
            {"variant_index": 0, "kind": "sse", "hits": 100, "weight": 80},
            {"variant_index": 1, "kind": "tcp_reset_mid_body",
             "hits": 100, "weight": 5},
        ],
    }
    by_error_code = {"GW_UPSTREAM_CONNECT_FAILED": 0, "GW_UPSTREAM_CANCELLED": 0}
    leaf = chaos_causal_per_variant(chaos_stats, by_error_code, min_ratio=0.7)
    assert leaf["raw"]["per_variant"]["tcp_reset_mid_body"]["passed"] is False
    assert leaf["passed"] is False


def test_min_completion_pass():
    rpt = SoakDriverReport.empty(target_count=49980)
    rpt.requests_completed = 30000
    leaf = min_completion_at_least_half(rpt)
    assert leaf["passed"] is True


def test_min_completion_fail():
    rpt = SoakDriverReport.empty(target_count=49980)
    rpt.requests_completed = 10000
    leaf = min_completion_at_least_half(rpt)
    assert leaf["passed"] is False


from soak.invariants import (
    soft_success_rate,
    soft_latency_p99,
    soft_completion_rate,
    soft_records_to_2xx_ratio,
    validate_variant_map_against_catalog,
)


def test_soft_success_rate_ok():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.by_status = {200: 100}
    leaf = soft_success_rate(rpt, chaos_mode="off")
    assert leaf["status"] == "ok"


def test_soft_success_rate_warn():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.by_status = {200: 50, 500: 50}
    leaf = soft_success_rate(rpt, chaos_mode="off")
    assert leaf["status"] == "warn"


def test_soft_latency_p99_ok():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.latency_ms_p99 = 150.0
    leaf = soft_latency_p99(rpt, chaos_mode="off")
    assert leaf["status"] == "ok"


def test_soft_latency_p99_warn():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.latency_ms_p99 = 300.0
    leaf = soft_latency_p99(rpt, chaos_mode="off")
    assert leaf["status"] == "warn"


def test_soft_completion_rate_ok():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.requests_completed = 95
    leaf = soft_completion_rate(rpt, chaos_mode="off")
    assert leaf["status"] == "ok"


def test_soft_records_to_2xx_warn_on_double_emit():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.by_status = {200: 10}
    leaf = soft_records_to_2xx_ratio(rpt, records_delta=15)
    assert leaf["status"] == "warn"
    assert leaf["raw"]["ratio"] == 1.5


def test_validate_variant_map_passes_for_full_catalog():
    full_catalog = {
        "GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_CANCELLED",
        "GW_UPSTREAM_TIMEOUT", "GW_UPSTREAM_OVERLOADED", "GW_UPSTREAM_5XX",
    }
    err = validate_variant_map_against_catalog(full_catalog)
    assert err is None


def test_validate_variant_map_fails_on_missing():
    err = validate_variant_map_against_catalog({"GW_UPSTREAM_TIMEOUT"})
    assert err is not None
    assert "GW_UPSTREAM_CONNECT_FAILED" in err
