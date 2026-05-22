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
