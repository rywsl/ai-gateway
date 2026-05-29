"""Unit tests for the kill-during-flight dedupe accounting (spec §8.3).

Pure-Python; no docker, no live services — runs under the default
`pytest tests/e2e/client/soak` invocation (soak/conftest.py no-ops the
live-service autouse fixtures).
"""
from soak.dedupe import count_dedupes, dedupe_breakdown


def _rec(rid, attempt, exposure="settled", out_prov="upstream_final"):
    return {
        "request_id": rid,
        "attempt_index": attempt,
        "vendor_exposure_state": exposure,
        "usage_provenance": {
            "input": "upstream_final",
            "output": out_prov,
            "cache_read": "unknown",
            "cache_creation": "unknown",
            "thinking": "unknown",
            "multimodal_input": "unknown",
        },
    }


def test_no_collisions_zero_dedupes():
    recs = [_rec("a", 0), _rec("b", 0), _rec("a", 1)]  # distinct keys
    assert count_dedupes(recs) == 0


def test_single_crashfallback_dup_counts_one():
    # A crash after XADD before DEL: settled winner + crash_fallback dup for
    # the same (request_id, attempt_index).
    recs = [
        _rec("a", 0, exposure="settled"),
        _rec("a", 0, exposure="crash_fallback", out_prov="crash_fallback"),
    ]
    assert count_dedupes(recs) == 1


def test_multiple_dups_same_key():
    recs = [_rec("a", 0), _rec("a", 0), _rec("a", 0)]  # group size 3
    assert count_dedupes(recs) == 2  # group_size - 1


def test_dups_isolated_per_attempt_index():
    # Same request_id but different attempt_index do NOT collide (a retry is a
    # distinct upstream hit, not a dup).
    recs = [
        _rec("a", 0, exposure="settled"),
        _rec("a", 0, exposure="crash_fallback"),  # dup of attempt 0
        _rec("a", 1, exposure="settled"),  # retry, distinct hit
    ]
    assert count_dedupes(recs) == 1


def test_missing_request_id_skipped():
    recs = [{"attempt_index": 0, "vendor_exposure_state": "settled"}, _rec("a", 0)]
    assert count_dedupes(recs) == 0


def test_breakdown_reports_winner_and_discarded():
    recs = [
        _rec("a", 0, exposure="crash_fallback", out_prov="crash_fallback"),
        _rec("a", 0, exposure="settled", out_prov="upstream_final"),
    ]
    bd = dedupe_breakdown(recs)
    assert bd["dedupes_inferred"] == 1
    assert bd["records_total"] == 2
    assert bd["distinct_request_attempt_keys"] == 1
    assert len(bd["collision_groups"]) == 1
    grp = bd["collision_groups"][0]
    # Highest-provenance (settled / upstream_final) must win.
    assert grp["winner"]["vendor_exposure_state"] == "settled"
    assert grp["discarded"][0]["vendor_exposure_state"] == "crash_fallback"
    # Winner rank strictly dominates the discarded one.
    assert grp["winner"]["rank"] > grp["discarded"][0]["rank"]


def test_exposure_dominates_dimension_provenance():
    # settled with a weak dimension still out-ranks crash_fallback with a
    # strong dimension — exposure_state is the primary key.
    strong_crash = _rec("a", 0, exposure="crash_fallback", out_prov="upstream_final")
    weak_settled = _rec("a", 0, exposure="settled", out_prov="unknown")
    bd = dedupe_breakdown([strong_crash, weak_settled])
    grp = bd["collision_groups"][0]
    assert grp["winner"]["vendor_exposure_state"] == "settled"


def test_empty_input():
    assert count_dedupes([]) == 0
    bd = dedupe_breakdown([])
    assert bd["dedupes_inferred"] == 0
    assert bd["records_total"] == 0
