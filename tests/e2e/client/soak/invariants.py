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

import re

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
    raw_state_after = after.get("State") or after.get("Status") or "unknown"
    if isinstance(raw_state_after, dict):
        state_after = raw_state_after.get("Status", "unknown")
    else:
        state_after = str(raw_state_after)
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


# ── Group 2: account_consistency (§7.1.2) ────────────────────────────────

def account_records_equals_upstream_hits(*, window: dict, hits: dict) -> dict:
    rs = window.get("redis_streams") or {}
    records = int(rs.get("usage_records_delta", 0))
    dlq = int(rs.get("usage_records_dlq_delta", 0))
    routes_sum = int(hits.get("routes_hit_sum", 0))
    refused_sum = int(hits.get("refused_hit_sum", 0))
    total = int(hits.get("total", routes_sum + refused_sum))
    diff = abs((records + dlq) - total)
    return _leaf(
        "results.invariants.account_consistency.records_equals_upstream_hits",
        kind="hard",
        passed=(diff == 0),
        source="fake_web:/soak/window/report + fake_upstream:/admin/hits-total",
        raw={
            "records_delta": records,
            "dlq_delta": dlq,
            "upstream_routes_hit_sum": routes_sum,
            "upstream_refused_hit_sum": refused_sum,
            "upstream_hits_total": total,
            "diff": diff,
            "drain_observation_series": window.get("drain_observation_series", []),
        },
        threshold="==",
    )


def account_balance_monotonic_decreasing(*, window: dict) -> dict:
    decs: dict[str, int] = window.get("balance_decrement_micros") or {}
    negative = [u for u, v in decs.items() if v < 0]
    return _leaf(
        "results.invariants.account_consistency.balance_monotonic_decreasing",
        kind="hard",
        passed=(len(negative) == 0),
        source="fake_web:/soak/window/report.balance_decrement_micros",
        raw={
            "user_count": len(decs),
            "negative_users": negative,
        },
    )


def account_drain_quiescence_reached(*, window: dict) -> dict:
    quiescent = bool(window.get("drain_quiescent", False))
    return _leaf(
        "results.invariants.account_consistency.drain_quiescence_reached",
        kind="hard",
        passed=quiescent,
        source="fake_web:/soak/window/report.drain_quiescent",
        raw={
            "drain_quiescent": quiescent,
            "drain_observation_series": window.get("drain_observation_series", []),
        },
    )


# ── Group 3: resource_release (§7.1.3) ───────────────────────────────────

_INFLIGHT_RE = re.compile(
    r"^(?P<series>gw_[a-zA-Z0-9_]*in_?flight[a-zA-Z0-9_]*(\{[^}]*\})?)\s+(?P<v>[-+0-9eE.]+)\s*$",
    re.MULTILINE,
)


def resource_all_in_flight_gauges_zero(metrics_text: str) -> dict:
    non_zero: dict[str, float] = {}
    for m in _INFLIGHT_RE.finditer(metrics_text):
        try:
            v = float(m.group("v"))
        except ValueError:
            continue
        if v != 0:
            non_zero[m.group("series")] = v
    return _leaf(
        "results.invariants.resource_release.all_in_flight_gauges_zero",
        kind="hard",
        passed=(len(non_zero) == 0),
        source="gateway:/metrics",
        raw={
            "scan_pattern": "^gw_*in_flight* series",
            "non_zero_gauges": non_zero,
        },
    )


def resource_rss_growth_under_threshold(
    *,
    before_bytes: int | None,
    after_bytes: int | None,
    threshold_mib: int = 100,
) -> dict:
    if before_bytes is None or after_bytes is None:
        return _leaf(
            "results.invariants.resource_release.rss_growth_under_threshold",
            kind="hard",
            passed=None,
            source="docker.stats.gateway",
            raw={
                "reason": f"RSS measurement unavailable: before={before_bytes}, after={after_bytes}",
                "before_bytes": before_bytes,
                "after_bytes": after_bytes,
            },
        )
    growth_bytes = max(0, after_bytes - before_bytes)
    growth_mib = growth_bytes / (1024 * 1024)
    return _leaf(
        "results.invariants.resource_release.rss_growth_under_threshold",
        kind="hard",
        passed=(growth_mib < threshold_mib),
        source="docker.stats.gateway",
        raw={
            "rss_before_bytes": before_bytes,
            "rss_after_bytes": after_bytes,
            "growth_bytes": growth_bytes,
            "growth_mib": round(growth_mib, 2),
            "threshold_mib": threshold_mib,
        },
    )


# ── Group 4: error_code_compliance (§7.1.4) ──────────────────────────────

def error_code_compliance_all_in_catalog(
    *,
    observed: set[str],
    catalog: set[str],
    catalog_source: str,
) -> dict:
    unknown = sorted(observed - catalog)
    return _leaf(
        "results.invariants.error_code_compliance.all_codes_in_catalog",
        kind="hard",
        passed=(len(unknown) == 0),
        source=f"driver.by_error_code ∪ fake_web.detailed_log_spectrum vs {catalog_source}",
        raw={
            "catalog_source": catalog_source,
            "observed": sorted(observed),
            "unknown": unknown,
        },
    )


# ── Group 5: chaos_causal_reconciliation (§7.1.5) ────────────────────────

# Spec Appendix A — pinned variant→error_code mapping.
VARIANT_ERROR_CODE_MAP: dict[str, list[str]] = {
    "tcp_reset_after_headers": ["GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_CANCELLED",
                                 "CLIENT_SIDE_FAILURE"],
    "tcp_reset_mid_body":      ["GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_CANCELLED",
                                 "CLIENT_SIDE_FAILURE"],
    "connection_refused_first_n": ["GW_UPSTREAM_CONNECT_FAILED"],
    "slow_body_byte_per_second":  ["GW_UPSTREAM_TIMEOUT", "CLIENT_SIDE_FAILURE"],
    "partial_sse_event": ["GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_TIMEOUT",
                          "GW_UPSTREAM_CANCELLED", "CLIENT_SIDE_FAILURE"],
    "status_with_retry_after": ["GW_UPSTREAM_OVERLOADED", "HTTP_STATUS_429"],
    "static":                  ["GW_UPSTREAM_5XX", "HTTP_STATUS_502"],
}


def chaos_causal_per_variant(
    chaos_stats: dict,
    by_error_code: dict[str, int],
    *,
    min_ratio: float = 0.7,
) -> dict:
    """Spec §7.1.5 — per-variant `E ≥ floor(H × min_ratio)`.

    The `sse` variant is skipped (the happy path is not a fault).
    Variants with `hits == 0` are also skipped (no causality to check).
    """
    per_variant: dict[str, dict] = {}
    overall_pass = True
    for v in chaos_stats.get("variants", []):
        kind = v.get("kind", "")
        hits = int(v.get("hits", 0))
        if kind == "sse" or hits == 0:
            continue
        mapped = VARIANT_ERROR_CODE_MAP.get(kind, [])
        matched = sum(by_error_code.get(c, 0) for c in mapped)
        required = int(hits * min_ratio)
        passed = matched >= required
        per_variant[kind] = {
            "variant_index": v.get("variant_index"),
            "variant_hits": hits,
            "mapped_error_codes": mapped,
            "matched_error_count": matched,
            "ratio": round(matched / hits, 3) if hits else None,
            "min_ratio": min_ratio,
            "passed": passed,
        }
        if not passed:
            overall_pass = False
    return _leaf(
        "results.invariants.chaos_causal_reconciliation.summary",
        kind="hard",
        passed=overall_pass if per_variant else True,
        source="fake_upstream:/admin/chaos-stats + driver.by_error_code",
        raw={"per_variant": per_variant},
    )


# ── Group 6: min_completion (§7.1.6) ─────────────────────────────────────

def min_completion_at_least_half(rpt: SoakDriverReport) -> dict:
    threshold = rpt.target_count * 0.5
    return _leaf(
        "results.invariants.min_completion.at_least_half_completed",
        kind="hard",
        passed=(rpt.requests_completed >= threshold),
        source="driver.requests_completed",
        raw={
            "requests_completed": rpt.requests_completed,
            "target_count": rpt.target_count,
            "threshold_count": int(threshold),
        },
    )


# ── Soft warnings (§7.2; thresholds §11.2) ───────────────────────────────

_SOFT_THRESHOLDS = {
    "off":   {"success_rate_2xx_pct": 99.9, "latency_p99_ms": 200,
              "completion_rate_pct": 90,  "thread_cpu_max_pct": 80,
              "chaos_variant_hit_deviation_pct": None,
              "records_to_2xx_ratio_max": 1.10},
    "light": {"success_rate_2xx_pct": 95.0, "latency_p99_ms": 300,
              "completion_rate_pct": 80,  "thread_cpu_max_pct": 80,
              "chaos_variant_hit_deviation_pct": 25,
              "records_to_2xx_ratio_max": 1.10},
    "heavy": {"success_rate_2xx_pct": 80.0, "latency_p99_ms": 500,
              "completion_rate_pct": 60,  "thread_cpu_max_pct": 80,
              "chaos_variant_hit_deviation_pct": 25,
              "records_to_2xx_ratio_max": 1.30},  # Empirical: observed 1.171 under heavy chaos (2026-05-25); allows ~30% billable non-2xx from upstream-attempted failures
}


def _soft_leaf(path: str, source: str, raw: dict, status: str) -> dict:
    return {"path": path, "kind": "soft", "status": status,
            "source": source, "raw": raw}


def soft_success_rate(rpt: SoakDriverReport, *, chaos_mode: str) -> dict:
    s2xx = sum(c for s, c in rpt.by_status.items() if 200 <= s < 300)
    completed = rpt.requests_completed or sum(rpt.by_status.values())
    pct = (s2xx / completed * 100.0) if completed else 0.0
    th = _SOFT_THRESHOLDS[chaos_mode]["success_rate_2xx_pct"]
    return _soft_leaf(
        "results.soft_warnings.success_rate_2xx_pct",
        "driver.by_status",
        {"actual_pct": round(pct, 2), "threshold_pct": th,
         "completed": completed, "count_2xx": s2xx},
        "ok" if pct >= th else "warn",
    )


def soft_latency_p99(rpt: SoakDriverReport, *, chaos_mode: str) -> dict:
    p99 = rpt.latency_ms_p99
    th = _SOFT_THRESHOLDS[chaos_mode]["latency_p99_ms"]
    status = "ok" if (p99 is not None and p99 < th) else "warn"
    return _soft_leaf(
        "results.soft_warnings.latency_p99_ms",
        "driver.latency_ms_p99",
        {"actual_ms": p99, "threshold_ms": th},
        status,
    )


def soft_completion_rate(rpt: SoakDriverReport, *, chaos_mode: str) -> dict:
    pct = (rpt.requests_completed / rpt.target_count * 100.0) if rpt.target_count else 0.0
    th = _SOFT_THRESHOLDS[chaos_mode]["completion_rate_pct"]
    return _soft_leaf(
        "results.soft_warnings.completion_rate_pct",
        "driver.requests_completed / target_count",
        {"actual_pct": round(pct, 2), "threshold_pct": th},
        "ok" if pct >= th else "warn",
    )


def soft_records_to_2xx_ratio(rpt: SoakDriverReport, *, chaos_mode: str, records_delta: int) -> dict:
    s2xx = sum(c for s, c in rpt.by_status.items() if 200 <= s < 300) or 1
    ratio = records_delta / s2xx
    th = _SOFT_THRESHOLDS[chaos_mode]["records_to_2xx_ratio_max"]
    return _soft_leaf(
        "results.soft_warnings.records_to_2xx_ratio",
        "fake_web.usage_records_delta / driver.by_status[2xx]",
        {"ratio": round(ratio, 3), "threshold_max": th,
         "records_delta": records_delta, "count_2xx": s2xx},
        "ok" if ratio <= th else "warn",
    )


# ── Startup validator: spec Appendix A vs codes.rs ───────────────────────

def validate_variant_map_against_catalog(catalog: set[str]) -> str | None:
    """Per spec §10 + C6 — every code in VARIANT_ERROR_CODE_MAP must be in
    `catalog` (parsed from codes.rs). Returns an error string if any
    mapping is stale, else None."""
    all_codes: set[str] = set()
    for codes in VARIANT_ERROR_CODE_MAP.values():
        all_codes.update(codes)
    all_codes = {c for c in all_codes if c.startswith("GW_")}
    missing = sorted(all_codes - catalog)
    if missing:
        return (
            f"variant→error_code mapping references codes not in codes.rs "
            f"catalog: {missing}. Update VARIANT_ERROR_CODE_MAP in "
            f"soak/invariants.py and Appendix A of the spec."
        )
    return None
