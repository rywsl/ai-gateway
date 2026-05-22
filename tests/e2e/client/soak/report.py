"""Report writer — produces the JSON tree + JSONL files (spec §8).

`compose_report` builds the in-memory tree from the invariant evaluators'
leaves plus the driver/window/spectra inputs. `write_reports` writes both
files atomically to the timestamped directory.
"""
from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

from soak.driver import SoakDriverReport


def _utc_ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M-%SZ")


def _set_path(root: dict, dotted: str, value: dict) -> None:
    """Set `root[a][b][c] = value` for dotted `a.b.c`. Creates dicts on the way."""
    parts = dotted.split(".")
    cur: dict = root
    for p in parts[:-1]:
        cur = cur.setdefault(p, {})
    cur[parts[-1]] = value


def compose_report(
    *,
    scenario: str,
    config: dict,
    timeline: dict,
    driver_report: SoakDriverReport,
    invariants: list[dict],
    soft_warnings: list[dict],
    spectra: dict,
) -> dict:
    root: dict = {"soak_run": {
        "id": f"{timeline.get('t0_iso', _utc_ts())}-{scenario}",
        "config": config,
        "timeline": timeline,
        "results": {
            "invariants": {},
            "soft_warnings": {},
            "spectra": spectra,
        },
    }}

    # Hard invariant leaves placed at their declared paths.
    for leaf in invariants:
        _set_path(root, leaf["path"], leaf)
    for leaf in soft_warnings:
        _set_path(root, leaf["path"], leaf)

    # Verdict.
    hard_total = len(invariants)
    failed = [l for l in invariants if l["passed"] is not True]
    root["soak_run"]["results"]["verdict"] = {
        "hard_invariants_total": hard_total,
        "hard_invariants_passed": hard_total - len(failed),
        "hard_invariants_failed": len(failed),
        "first_failure_path": failed[0]["path"] if failed else None,
        "all_failures": [l["path"] for l in failed],
        "overall": "PASS" if not failed else "FAIL",
    }
    # Driver-side latency/throughput summary parked under spectra.
    root["soak_run"]["results"]["spectra"]["driver_summary"] = {
        "source": "driver",
        "raw": {
            "duration_ms": driver_report.duration_ms,
            "requests_sent": driver_report.requests_sent,
            "requests_completed": driver_report.requests_completed,
            "requests_failed_client_side": driver_report.requests_failed_client_side,
            "by_status": driver_report.by_status,
            "by_error_code": driver_report.by_error_code,
            "by_protocol": driver_report.by_protocol,
            "by_shape": driver_report.by_shape,
            "by_cross_proto": {str(k): v for k, v in driver_report.by_cross_proto.items()},
            "latency_ms": {
                "p50": driver_report.latency_ms_p50,
                "p90": driver_report.latency_ms_p90,
                "p99": driver_report.latency_ms_p99,
                "max": driver_report.latency_ms_max,
            },
            "bytes_received_total": driver_report.bytes_received_total,
        },
    }
    return root


def write_reports(
    out_dir: Path,
    *,
    scenario: str,
    tree: dict,
    leaves: list[dict],
) -> dict:
    """Write `<out_dir>/<ts>-<scenario>.json` + `.jsonl`. Returns the paths."""
    out_dir.mkdir(parents=True, exist_ok=True)
    ts = _utc_ts()
    json_path = out_dir / f"{ts}-{scenario}.json"
    jsonl_path = out_dir / f"{ts}-{scenario}.jsonl"

    json_path.write_text(json.dumps(tree, indent=2, sort_keys=False))

    with jsonl_path.open("w") as f:
        config = tree["soak_run"].get("config", {})
        f.write(json.dumps({"path": "results.config", "data": config}) + "\n")
        for leaf in leaves:
            f.write(json.dumps(leaf) + "\n")
        for k, v in tree["soak_run"]["results"].get("spectra", {}).items():
            f.write(json.dumps({"path": f"results.spectra.{k}", "data": v}) + "\n")
        f.write(json.dumps({"path": "results.verdict",
                            **tree["soak_run"]["results"]["verdict"]}) + "\n")
    return {"json_path": json_path, "jsonl_path": jsonl_path}
