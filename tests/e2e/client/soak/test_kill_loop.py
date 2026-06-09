"""Unit tests for the kill-loop injector + kill-mode invariants (spec §8.3).

Pure-Python; no docker. The KillLoop is exercised only in its DISABLED state
(prob_per_s == 0) so no `docker compose kill` ever shells out — the enabled
path is covered end-to-end by `make e2e-soak-kill`. The invariant evaluators
are exercised with hand-built window/hits/records fixtures.
"""
from soak.kill_loop import KillLoop, KillLoopReport
from soak.invariants import (
    account_records_equals_upstream_hits_dedupe_aware,
    kill_loop_fired_at_least_once,
)


# ── KillLoop (disabled) ──────────────────────────────────────────────────

def test_kill_loop_disabled_is_noop():
    loop = KillLoop(prob_per_s=0.0)
    loop.start()  # spawns no thread
    rpt = loop.stop()
    assert isinstance(rpt, KillLoopReport)
    assert rpt.enabled is False
    assert rpt.kills_attempted == 0
    assert rpt.events == []


# ── dedupe-aware account identity ─────────────────────────────────────────

def _window(records_delta, dlq_delta=0):
    return {"redis_streams": {"usage_records_delta": records_delta,
                              "usage_records_dlq_delta": dlq_delta}}


def _hits(total):
    return {"routes_hit_sum": total, "refused_hit_sum": 0, "total": total}


def _crashfallback_pair(rid):
    """A settled winner + a crash_fallback dup for the same attempt 0."""
    base = {
        "request_id": rid, "attempt_index": 0,
        "usage_provenance": {k: "unknown" for k in
                             ("input", "output", "cache_read", "cache_creation",
                              "thinking", "multimodal_input")},
    }
    return [
        {**base, "vendor_exposure_state": "settled"},
        {**base, "vendor_exposure_state": "crash_fallback"},
    ]


def test_dedupe_aware_passes_when_dups_explain_overcount():
    # 3 real hits, but the stream has 4 records because one attempt got a
    # CrashFallback duplicate. records(4) + dlq(0) - dedupes(1) == hits(3).
    entries = (
        [{"request_id": "a", "attempt_index": 0, "vendor_exposure_state": "settled",
          "usage_provenance": {}}]
        + [{"request_id": "b", "attempt_index": 0, "vendor_exposure_state": "settled",
            "usage_provenance": {}}]
        + _crashfallback_pair("c")
    )
    leaf = account_records_equals_upstream_hits_dedupe_aware(
        window=_window(4), hits=_hits(3), records_stream_entries=entries,
    )
    assert leaf["passed"] is True
    assert leaf["raw"]["crashfallback_dedupes_inferred"] == 1
    assert leaf["raw"]["records_minus_dedupes"] == 3
    assert leaf["raw"]["diff"] == 0


def test_dedupe_aware_fails_on_unexplained_overcount():
    # records overcount by 2 but only 1 dup is inferable → genuine drift.
    entries = _crashfallback_pair("c")  # 2 records, 1 dup
    leaf = account_records_equals_upstream_hits_dedupe_aware(
        window=_window(2), hits=_hits(0), records_stream_entries=entries,
    )
    # 2 + 0 - 1 = 1 != 0 → fail.
    assert leaf["passed"] is False
    assert leaf["raw"]["diff"] == 1


def test_dedupe_aware_no_dups_matches_strict_identity():
    entries = [
        {"request_id": "a", "attempt_index": 0, "vendor_exposure_state": "settled",
         "usage_provenance": {}},
        {"request_id": "b", "attempt_index": 0, "vendor_exposure_state": "settled",
         "usage_provenance": {}},
    ]
    leaf = account_records_equals_upstream_hits_dedupe_aware(
        window=_window(2), hits=_hits(2), records_stream_entries=entries,
    )
    assert leaf["passed"] is True
    assert leaf["raw"]["crashfallback_dedupes_inferred"] == 0


# ── kill-fired invariant ──────────────────────────────────────────────────

def test_kill_fired_passes_when_at_least_one_succeeded():
    leaf = kill_loop_fired_at_least_once(kill_report={"kills_succeeded": 3,
                                                      "kills_attempted": 3})
    assert leaf["passed"] is True


def test_kill_fired_fails_when_zero_kills():
    leaf = kill_loop_fired_at_least_once(kill_report={"kills_succeeded": 0})
    assert leaf["passed"] is False


def test_dedupe_rank_orders_cli_extracted_between_final_and_partial():
    from soak.dedupe import _PROVENANCE_RANK

    assert _PROVENANCE_RANK["upstream_final"] == 5
    assert _PROVENANCE_RANK["cli_extracted"] == 4
    assert _PROVENANCE_RANK["upstream_partial"] == 3
    assert _PROVENANCE_RANK["estimated"] == 2
    assert _PROVENANCE_RANK["crash_fallback"] == 1
    assert _PROVENANCE_RANK["unknown"] == 0


def test_dedupe_breakdown_keeps_cli_extracted_over_estimated_fallback():
    from soak.dedupe import dedupe_breakdown

    def rec(provenance):
        return {
            "request_id": "cli-usage-1",
            "attempt_index": 0,
            "vendor_exposure_state": "settled",
            "usage_provenance": {
                "input": provenance,
                "output": "unknown",
                "cache_read": "unknown",
                "cache_creation": "unknown",
                "thinking": "unknown",
                "multimodal_input": "unknown",
            },
        }

    breakdown = dedupe_breakdown([rec("estimated"), rec("cli_extracted")])

    assert breakdown["dedupes_inferred"] == 1
    [collision] = breakdown["collision_groups"]
    assert collision["winner"]["rank"] == [3, 4]
    assert collision["discarded"][0]["rank"] == [3, 2]
