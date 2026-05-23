import json
from pathlib import Path

from soak.driver import SoakDriverReport
from soak.report import compose_report, write_reports


def _sample_leaf(path: str, passed: bool) -> dict:
    return {"path": path, "kind": "hard", "passed": passed,
            "source": "test", "raw": {}}


def test_compose_report_builds_tree_with_verdict():
    invariants = [
        _sample_leaf("results.invariants.process_alive.no_panic", True),
        _sample_leaf("results.invariants.account_consistency.records_equals_upstream_hits", False),
        _sample_leaf("results.invariants.resource_release.all_in_flight_gauges_zero", True),
    ]
    soft_warnings: list[dict] = []
    spectra: dict[str, dict] = {}
    rpt = SoakDriverReport.empty(target_count=49980)
    rpt.duration_ms = 60123
    tree = compose_report(
        scenario="chaos-heavy-60s",
        config={"duration_s": 60, "target_rps": 833, "chaos_mode": "heavy"},
        timeline={"t0_iso": "2026-05-22T14:30:00Z",
                  "driver_t1_iso": "2026-05-22T14:31:00Z"},
        driver_report=rpt,
        invariants=invariants,
        soft_warnings=soft_warnings,
        spectra=spectra,
    )
    assert tree["soak_run"]["config"]["chaos_mode"] == "heavy"
    v = tree["soak_run"]["results"]["verdict"]
    assert v["overall"] == "FAIL"
    assert v["hard_invariants_total"] == 3
    assert v["hard_invariants_failed"] == 1
    assert v["first_failure_path"] == \
        "results.invariants.account_consistency.records_equals_upstream_hits"


def test_write_reports_emits_json_and_jsonl(tmp_path: Path):
    tree = {"soak_run": {"id": "x", "results": {
        "invariants": {}, "verdict": {"overall": "PASS"}}}}
    out = write_reports(tmp_path, scenario="smoke", tree=tree, leaves=[
        {"path": "results.invariants.x", "kind": "hard", "passed": True,
         "source": "t", "raw": {}}
    ])
    assert out["json_path"].exists()
    assert out["jsonl_path"].exists()
    json.loads(out["json_path"].read_text())  # valid JSON
    lines = out["jsonl_path"].read_text().splitlines()
    assert len(lines) >= 2  # config + verdict + at least one leaf
    for line in lines:
        json.loads(line)  # each line is valid JSON
