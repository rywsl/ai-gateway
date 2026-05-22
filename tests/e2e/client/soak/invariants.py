"""Invariant evaluators for the soak harness — spec §7.

Each function returns a dict matching the report JSON leaf shape:

    {
      "path":   "results.invariants.<group>.<name>",
      "kind":   "hard" | "soft",
      "passed": bool | None,        # None means "data unavailable"
      "source": "<concrete endpoint/method>",
      "raw":    { ...evidence... },
      "threshold": "<optional, when not obvious from raw>",
    }

Per spec C4, every leaf has a non-null `source` and `raw` carries the
raw observation. Missing data is `passed: None` with `raw.reason`.
"""
from __future__ import annotations

from soak.driver import SoakDriverReport
from soak.log_scan import PANIC_PATTERN, scan_panic_lines


def _leaf(path: str, *, kind: str, passed: bool | None, source: str,
          raw: dict, threshold: str | None = None) -> dict:
    out = {
        "path": path,
        "kind": kind,
        "passed": passed,
        "source": source,
        "raw": raw,
    }
    if threshold is not None:
        out["threshold"] = threshold
    return out


# ── Group 1: process_alive (§7.1.1) ──────────────────────────────────────

def process_alive_no_panic(gateway_log_text: str) -> dict:
    matched = scan_panic_lines(gateway_log_text)
    return _leaf(
        "results.invariants.process_alive.no_panic",
        kind="hard",
        passed=(len(matched) == 0),
        source="docker.compose_logs.gateway",
        raw={
            "scan_pattern": PANIC_PATTERN,
            "matched_lines": matched,
            "match_count": len(matched),
        },
    )


def process_alive_livez_throughout(rpt: SoakDriverReport) -> dict:
    return _leaf(
        "results.invariants.process_alive.livez_200_throughout",
        kind="hard",
        passed=(rpt.livez_non_200 == 0 and rpt.livez_probes > 0),
        source="driver.livez_probe",
        raw={"probes": rpt.livez_probes, "non_200_count": rpt.livez_non_200},
    )


def process_alive_readyz_throughout(rpt: SoakDriverReport) -> dict:
    return _leaf(
        "results.invariants.process_alive.readyz_200_throughout",
        kind="hard",
        passed=(rpt.readyz_non_200 == 0 and rpt.readyz_probes > 0),
        source="driver.readyz_probe",
        raw={"probes": rpt.readyz_probes, "non_200_count": rpt.readyz_non_200},
    )


def process_alive_no_container_restart(*, before: dict, after: dict) -> dict:
    rc_before = int(before.get("RestartCount", 0))
    rc_after = int(after.get("RestartCount", 0))
    state_after = (after.get("State") or {}).get("Status", "unknown")
    passed = (rc_after - rc_before == 0) and state_after == "running"
    return _leaf(
        "results.invariants.process_alive.no_container_restart",
        kind="hard",
        passed=passed,
        source="docker.compose_ps.gateway",
        raw={
            "restart_count_before": rc_before,
            "restart_count_after": rc_after,
            "restart_count_delta": rc_after - rc_before,
            "state_after": state_after,
        },
    )
