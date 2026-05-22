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
