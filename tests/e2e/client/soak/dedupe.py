"""Dedupe-aware accounting for the kill-during-flight soak mode — spec §8.3.

When the gateway is SIGKILLed mid-flight, the billing-record-durability fix
guarantees no usage record is LOST (the inflight key survives a crash before
XADD and is recovered by the Go reconciler as a `CrashFallback` record).  The
flip side is that a crash AFTER XADD but BEFORE the inflight DEL leaves the key
present, so the reconciler ALSO emits a `CrashFallback` record for the same
attempt — a DUPLICATE that Go discards by `(request_id, attempt_index)` +
provenance level.

The strict-equality account-consistency invariant the steady-state soak relies
on (`records_delta + dlq_delta == upstream_hits_total`) therefore does NOT hold
under kill chaos: the duplicates inflate `records_delta` above the true
upstream-hit count.  This module computes `crashfallback_dedupes_inferred` —
the number of `usage:records` entries that share `(request_id, attempt_index)`
with a HIGHER-provenance sibling in the same stream — so the kill-mode
invariant can be stated as:

    records_delta + dlq_delta - crashfallback_dedupes_inferred == upstream_hits_total

i.e. once the Go-discarded duplicates are removed, the identity is restored.

Provenance ranking (highest = winner = the entry Go keeps; lowest = the dup
Go discards), per spec §8.3:

- `vendor_exposure_state`:  settled > terminated > crash_fallback
- `usage_provenance.<dim>` (any dimension): upstream_final > upstream_partial >
  estimated > crash_fallback > unknown

The record wire shape is `crates/gw-ledger/src/usage.rs::UsageRecord`
serialized to JSON under the `payload` field of each `usage:records` XADD
(see `crates/gw-ledger/src/flusher.rs::flush`).  All enum tags are
serde `rename_all = "snake_case"`.
"""
from __future__ import annotations

from typing import Iterable


# ── Provenance rankings (higher int = higher confidence = winner) ────────────

# `VendorExposureState` — crates/gw-ledger/src/usage.rs.
_EXPOSURE_RANK: dict[str, int] = {
    "settled": 3,
    "terminated": 2,
    "crash_fallback": 1,
}

# `ProvenanceTag` — crates/gw-core/src/usage.rs.
_PROVENANCE_RANK: dict[str, int] = {
    "upstream_final": 5,
    "upstream_partial": 4,
    "estimated": 3,
    "crash_fallback": 2,
    "unknown": 1,
}

# Dimensions carried by `UsageProvenance` (crates/gw-ledger/src/usage.rs).
_PROVENANCE_DIMS = (
    "input",
    "output",
    "cache_read",
    "cache_creation",
    "thinking",
    "multimodal_input",
)


def _record_rank(rec: dict) -> tuple[int, int]:
    """Provenance rank of one decoded `usage:records` payload.

    Returns `(exposure_rank, max_dimension_provenance_rank)`. Lexicographic
    ordering of the tuple matches spec §8.3: exposure_state dominates; the
    per-dimension provenance breaks ties (we take the MAX dimension rank,
    since "any dimension" with a higher provenance marks the stronger record).
    Unknown / missing tags rank lowest (0) so a malformed record never
    out-ranks a well-formed sibling.
    """
    exposure = _EXPOSURE_RANK.get(str(rec.get("vendor_exposure_state", "")), 0)
    prov = rec.get("usage_provenance") or {}
    dim_ranks = [
        _PROVENANCE_RANK.get(str(prov.get(d, "")), 0) for d in _PROVENANCE_DIMS
    ]
    max_dim = max(dim_ranks) if dim_ranks else 0
    return (exposure, max_dim)


def count_dedupes(records: Iterable[dict]) -> int:
    """Count `usage:records` entries that are duplicates Go would discard.

    `records` is an iterable of DECODED record payloads (each a dict with at
    least `request_id`, `attempt_index`, `vendor_exposure_state`, and
    `usage_provenance`).  Two entries collide when they share
    `(request_id, attempt_index)`.  For each collision group of size N, the
    single highest-provenance entry is the WINNER Go keeps; the remaining
    N-1 are the dups Go discards.  The returned count is the total number of
    discarded dups across all groups:

        sum over groups of (group_size - 1)

    Entries missing `request_id` are skipped (they cannot collide on a
    well-defined key).  Ties on full provenance rank still count the extras
    as dups — Go's dedup is deterministic on `(request_id, attempt_index)`
    regardless of how the tie is broken, so the COUNT of discards is
    rank-tie-independent.
    """
    groups: dict[tuple[str, int], int] = {}
    for rec in records:
        rid = rec.get("request_id")
        if rid is None:
            continue
        try:
            attempt = int(rec.get("attempt_index", 0))
        except (TypeError, ValueError):
            attempt = 0
        key = (str(rid), attempt)
        groups[key] = groups.get(key, 0) + 1

    return sum(size - 1 for size in groups.values() if size > 1)


def dedupe_breakdown(records: Iterable[dict]) -> dict:
    """Return a richer breakdown for the invariant's `raw` evidence block.

    Provides the aggregate `dedupes_inferred` count plus per-group detail for
    every COLLIDING `(request_id, attempt_index)` so a failing run is
    debuggable from the JSON report alone (which record won, which were
    discarded, and their provenance ranks).
    """
    by_key: dict[tuple[str, int], list[dict]] = {}
    total = 0
    skipped_no_rid = 0
    for rec in records:
        total += 1
        rid = rec.get("request_id")
        if rid is None:
            skipped_no_rid += 1
            continue
        try:
            attempt = int(rec.get("attempt_index", 0))
        except (TypeError, ValueError):
            attempt = 0
        by_key.setdefault((str(rid), attempt), []).append(rec)

    collisions = []
    dedupes = 0
    for (rid, attempt), recs in by_key.items():
        if len(recs) <= 1:
            continue
        dedupes += len(recs) - 1
        ranked = sorted(recs, key=_record_rank, reverse=True)
        winner = ranked[0]
        losers = ranked[1:]
        collisions.append({
            "request_id": rid,
            "attempt_index": attempt,
            "group_size": len(recs),
            "winner": {
                "vendor_exposure_state": winner.get("vendor_exposure_state"),
                "rank": list(_record_rank(winner)),
            },
            "discarded": [
                {
                    "vendor_exposure_state": l.get("vendor_exposure_state"),
                    "rank": list(_record_rank(l)),
                }
                for l in losers
            ],
        })

    return {
        "records_total": total,
        "records_skipped_no_request_id": skipped_no_rid,
        "distinct_request_attempt_keys": len(by_key),
        "dedupes_inferred": dedupes,
        "collision_groups": collisions,
    }
