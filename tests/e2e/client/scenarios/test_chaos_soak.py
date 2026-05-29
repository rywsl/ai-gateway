"""Soak / chaos orchestrator — spec §6.6 + §12.1.

Three test functions, all gated by the `@pytest.mark.soak` marker
(excluded from default pytest invocations via pytest.ini addopts).

Verification gate (spec C6): the green light from `test_soak_smoke`
is NOT sufficient evidence that the soak feature works.
`test_soak_chaos_off_60s` AND `test_soak_chaos_heavy_60s` must both
produce reports with `verdict.overall == "PASS"`.
"""
from __future__ import annotations

import os
import subprocess
import time
from dataclasses import replace
from datetime import datetime, timezone
from pathlib import Path

import pytest
import requests

from conftest import (
    FakeUpstreamClient,
    FakeWebClient,
    gateway_compose_logs_since,
    gateway_container_state,
    gateway_metrics_text,
    gateway_rss_bytes,
)
from soak.chaos_profiles import happy_scenario, profile_for, to_scenario
from soak.codes_catalog import parse_codes_catalog
from soak.driver import SoakConfig, SoakDriver
from soak.invariants import (
    account_balance_monotonic_decreasing,
    account_drain_quiescence_reached,
    account_records_equals_upstream_hits,
    account_records_equals_upstream_hits_dedupe_aware,
    chaos_causal_per_variant,
    error_code_compliance_all_in_catalog,
    kill_loop_fired_at_least_once,
    min_completion_at_least_half,
    process_alive_livez_throughout,
    process_alive_no_container_restart,
    process_alive_no_panic,
    process_alive_readyz_throughout,
    resource_all_in_flight_gauges_zero,
    resource_rss_growth_under_threshold,
    soft_completion_rate,
    soft_latency_p99,
    soft_records_to_2xx_ratio,
    soft_success_rate,
    validate_variant_map_against_catalog,
)
from soak.report import compose_report, write_reports
from soak.seed import seed_soak

# parents: [0]=scenarios, [1]=client, [2]=e2e, [3]=tests, [4]=worktree-root
REPO_ROOT = Path(__file__).resolve().parents[4]
SOAK_REPORT_DIR = Path(__file__).resolve().parents[1] / ".soak_report"


def _git_sha() -> str:
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "--short", "HEAD"],
            text=True, timeout=5,
        ).strip()
    except Exception:
        return "unknown"


def _read_usage_records(redis_url: str) -> list[dict]:
    """XRANGE `usage:records` and return the decoded payload of every entry.

    The gateway XADDs each record under a `payload=<json>` field
    (`crates/gw-ledger/src/flusher.rs::flush`). Redis is host-published on
    `localhost:6379` by `tests/e2e/docker-compose.yml`, so the host-side
    orchestrator can read the full stream directly — the fake-web window
    report only exposes the XLEN delta, not the per-record body needed for
    `(request_id, attempt_index)` dedupe inference (spec §8.3).
    """
    import json as _json
    import redis  # local import: kill-mode only

    client = redis.Redis.from_url(redis_url, decode_responses=True)
    try:
        entries = client.xrange("usage:records", min="-", max="+")
    finally:
        try:
            client.close()
        except Exception:
            pass
    out: list[dict] = []
    for _entry_id, fields in entries:
        payload = fields.get("payload")
        if payload is None:
            continue
        try:
            out.append(_json.loads(payload))
        except ValueError:
            continue
    return out


def _run_soak(
    *,
    cfg: SoakConfig,
    scenario_label: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    gateway_url: str,
    metrics_url: str,
    redis_url: str = "redis://localhost:6379",
) -> dict:
    """Drive one soak run end-to-end and return the report tree."""

    catalog = parse_codes_catalog(REPO_ROOT / "crates" / "gw-core" / "src" / "error" / "codes.rs")
    err = validate_variant_map_against_catalog(catalog)
    assert err is None, err

    handles = seed_soak(fake_web, n_users=cfg.n_users)
    profile = profile_for(cfg.chaos_mode)
    fake_upstream.set_scenario(
        happy_scenario() if profile is None else to_scenario(profile)
    )

    snap = requests.post(
        f"{fake_web.base}/soak/window/start",
        json={"user_ids": [u[0] for u in handles.users]},
        timeout=10,
    )
    snap.raise_for_status()
    snapshot_id = snap.json()["snapshot_id"]
    t0_iso = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    rss_before = gateway_rss_bytes()
    container_before = gateway_container_state()

    driver_cfg = SoakConfig(
        duration_s=cfg.duration_s,
        target_rps=cfg.target_rps,
        n_threads=cfg.n_threads,
        max_inflight_per_thread=cfg.max_inflight_per_thread,
        per_request_timeout_s=cfg.per_request_timeout_s,
        drain_grace_s=cfg.drain_grace_s,
        drain_quiescence_s=cfg.drain_quiescence_s,
        drain_grace_max_s=cfg.drain_grace_max_s,
        n_users=cfg.n_users,
        chaos_mode=cfg.chaos_mode,
        kill_during_flight=cfg.kill_during_flight,
        gateway_url=gateway_url,
        livez_url="http://localhost:8080/livez",
        readyz_url="http://localhost:8080/readyz",
    )
    driver_report = SoakDriver(driver_cfg, handles).run()
    t1_iso = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    window_resp = requests.get(
        f"{fake_web.base}/soak/window/report",
        params={"snapshot_id": snapshot_id},
        timeout=cfg.drain_grace_max_s + 30,
    )
    window_resp.raise_for_status()
    window = window_resp.json()

    hits = requests.get(f"{fake_upstream.admin}/admin/hits-total", timeout=10).json()
    chaos_stats = (
        requests.get(f"{fake_upstream.admin}/admin/chaos-stats", timeout=10).json()
        if profile is not None else {"enabled": False, "variants": []}
    )
    metrics_text = gateway_metrics_text(metrics_url)
    rss_after = gateway_rss_bytes()
    container_after = gateway_container_state()
    log_text = gateway_compose_logs_since(t0_iso)

    by_error_code = dict(driver_report.by_error_code)
    by_error_code.update(window["detailed_log_spectrum"].get("by_error_code", {}))
    for status, count in driver_report.by_status.items():
        if status >= 400:
            by_error_code[f"HTTP_STATUS_{status}"] = (
                by_error_code.get(f"HTTP_STATUS_{status}", 0) + count
            )
    if driver_report.requests_failed_client_side:
        by_error_code["CLIENT_SIDE_FAILURE"] = driver_report.requests_failed_client_side
    observed_codes = set(driver_report.by_error_code) | set(
        c for c in window["detailed_log_spectrum"].get("by_error_code", {}) if c != "__absent__"
    )

    kill_mode = cfg.kill_during_flight > 0.0
    # Under SIGKILL chaos the gateway intentionally dies + restarts, so the
    # liveness / no-restart invariants do NOT apply, the strict-equality
    # account identity is broken by reconciler-emitted CrashFallback dups, and
    # we read the full `usage:records` stream to infer those dups (spec §8.3).
    records_stream_entries = _read_usage_records(redis_url) if kill_mode else []

    invariants = [
        process_alive_no_panic(log_text),
    ]
    if not kill_mode:
        invariants += [
            process_alive_livez_throughout(driver_report),
            process_alive_readyz_throughout(driver_report),
            process_alive_no_container_restart(before=container_before, after=container_after),
            account_records_equals_upstream_hits(window=window, hits=hits),
        ]
    else:
        invariants += [
            account_records_equals_upstream_hits_dedupe_aware(
                window=window, hits=hits,
                records_stream_entries=records_stream_entries,
            ),
            kill_loop_fired_at_least_once(
                kill_report=driver_report.kill_report or {},
            ),
        ]
    invariants += [
        account_balance_monotonic_decreasing(window=window),
        account_drain_quiescence_reached(window=window),
        resource_all_in_flight_gauges_zero(metrics_text),
        resource_rss_growth_under_threshold(
            before_bytes=rss_before,
            after_bytes=rss_after,
            threshold_mib=100,
        ),
        error_code_compliance_all_in_catalog(
            observed=observed_codes,
            catalog=catalog,
            catalog_source=f"codes.rs@{_git_sha()}",
        ),
        min_completion_at_least_half(driver_report),
    ]
    if profile is not None:
        invariants.append(
            chaos_causal_per_variant(chaos_stats, by_error_code, min_ratio=0.7)
        )

    soft_warnings = [
        soft_success_rate(driver_report, chaos_mode=cfg.chaos_mode),
        soft_latency_p99(driver_report, chaos_mode=cfg.chaos_mode),
        soft_completion_rate(driver_report, chaos_mode=cfg.chaos_mode),
        soft_records_to_2xx_ratio(
            driver_report,
            records_delta=int(window["redis_streams"]["usage_records_delta"]),
            chaos_mode=cfg.chaos_mode,
        ),
    ]

    spectra = {
        "by_status":      {"source": "driver",   "raw": driver_report.by_status},
        "by_error_code":  {"source": "driver+fake_web", "raw": by_error_code},
        "by_protocol":    {"source": "driver",   "raw": driver_report.by_protocol},
        "by_shape":       {"source": "driver",   "raw": driver_report.by_shape},
        "by_cross_proto": {"source": "driver",
                           "raw": {str(k): v for k, v in driver_report.by_cross_proto.items()}},
        "fake_web_window": {"source": "fake_web:/soak/window/report", "raw": window},
        "fake_upstream_hits": {"source": "fake_upstream:/admin/hits-total", "raw": hits},
        "fake_upstream_chaos": {"source": "fake_upstream:/admin/chaos-stats", "raw": chaos_stats},
    }
    if kill_mode:
        spectra["kill_loop"] = {
            "source": "soak.kill_loop.KillLoop.report",
            "raw": driver_report.kill_report or {},
        }
        spectra["usage_records_stream"] = {
            "source": "redis.xrange(usage:records)",
            "raw": {"entry_count": len(records_stream_entries)},
        }

    tree = compose_report(
        scenario=scenario_label,
        config={
            "duration_s": cfg.duration_s,
            "target_rps": cfg.target_rps,
            "n_threads": cfg.n_threads,
            "max_inflight_per_thread": cfg.max_inflight_per_thread,
            "n_users": cfg.n_users,
            "chaos_mode": cfg.chaos_mode,
            "kill_during_flight": cfg.kill_during_flight,
            "git_sha": _git_sha(),
            "gateway_image_sha": container_after.get("Image"),
        },
        timeline={
            "t0_iso": t0_iso,
            "driver_t1_iso": t1_iso,
            "drain_quiescent_at_iso": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "drain_wall_ms": window["window_ms"],
        },
        driver_report=driver_report,
        invariants=invariants,
        soft_warnings=soft_warnings,
        spectra=spectra,
    )
    write_reports(SOAK_REPORT_DIR, scenario=scenario_label,
                  tree=tree, leaves=invariants + soft_warnings)
    return tree


@pytest.mark.soak
def test_soak_smoke(fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient,
                    gateway_url: str, metrics_url: str) -> None:
    """Wiring validation only. NOT a feature-completion signal (spec C6)."""
    tree = _run_soak(
        cfg=SoakConfig.smoke(),
        scenario_label="smoke",
        fake_web=fake_web,
        fake_upstream=fake_upstream,
        gateway_url=gateway_url,
        metrics_url=metrics_url,
    )
    verdict = tree["soak_run"]["results"]["verdict"]
    assert verdict["overall"] == "PASS", verdict["first_failure_path"]


@pytest.mark.soak
def test_soak_chaos_off_60s(fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient,
                             gateway_url: str, metrics_url: str) -> None:
    """Full 833 rps × 60 s baseline, chaos off."""
    cfg = SoakConfig(chaos_mode="off")
    tree = _run_soak(cfg=cfg, scenario_label="chaos-off-60s",
                     fake_web=fake_web, fake_upstream=fake_upstream,
                     gateway_url=gateway_url, metrics_url=metrics_url)
    verdict = tree["soak_run"]["results"]["verdict"]
    assert verdict["overall"] == "PASS", verdict["first_failure_path"]


@pytest.mark.soak
def test_soak_chaos_heavy_60s(fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient,
                               gateway_url: str, metrics_url: str) -> None:
    """Full 833 rps × 60 s × HEAVY chaos profile. Bug-finding workload."""
    cfg = SoakConfig(chaos_mode="heavy")
    tree = _run_soak(cfg=cfg, scenario_label="chaos-heavy-60s",
                     fake_web=fake_web, fake_upstream=fake_upstream,
                     gateway_url=gateway_url, metrics_url=metrics_url)
    verdict = tree["soak_run"]["results"]["verdict"]
    assert verdict["overall"] == "PASS", verdict["first_failure_path"]


@pytest.mark.soak
def test_soak_chaos_kill_60s(fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient,
                             gateway_url: str, metrics_url: str, redis_url: str) -> None:
    """Kill-during-flight durability gate (spec §8.3).

    Drives a heavy-chaos run while a background loop SIGKILLs the gateway with
    probability `SOAK_KILL_PROB`/s (default 0.1, set by `make e2e-soak-kill`).
    Asserts the dedupe-aware account identity holds after the Go reconciler
    recovers / dedups every crash-affected attempt:

        records_delta + dlq_delta - crashfallback_dedupes_inferred
            == upstream_hits_total

    The strict-equality identity, liveness, and no-restart invariants are
    deliberately NOT applied here (the container is intentionally crashing) —
    `_run_soak` swaps them out when `kill_during_flight > 0`.

    Smoke-sized when `SOAK_SMOKE=1` so the driver/seed wiring can be iterated
    without a full 60s run; the kill probability still applies.
    """
    kill_prob = float(os.environ.get("SOAK_KILL_PROB", "0.1"))
    base = SoakConfig.smoke() if os.environ.get("SOAK_SMOKE") else SoakConfig()
    cfg = replace(base, chaos_mode="heavy", kill_during_flight=kill_prob)
    tree = _run_soak(cfg=cfg, scenario_label="chaos-kill-60s",
                     fake_web=fake_web, fake_upstream=fake_upstream,
                     gateway_url=gateway_url, metrics_url=metrics_url,
                     redis_url=redis_url)
    verdict = tree["soak_run"]["results"]["verdict"]
    assert verdict["overall"] == "PASS", verdict["first_failure_path"]
