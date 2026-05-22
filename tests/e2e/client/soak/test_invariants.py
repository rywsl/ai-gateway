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
