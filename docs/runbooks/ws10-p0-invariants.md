# WS-10 P0 Invariant Runbook

This runbook maps the P0 launch-gate invariants from the master plan to the
operational signal an on-call engineer should use first. It complements
`ws10-ops-alerts.md`; alert-specific steps remain in that file.

## WS-1 Money And Idempotency

**IS**: Any invariant that could cause a missed debit, duplicate debit, refund
overrun, orphaned payment/refund outbox item, or leaked Rust freeze/lease.

**IS NOT**: A delayed but still-retrying outbox item younger than its retry
window.

**WHY**: WS-1 removes commit-then-side-effect bugs for wallet, payment, refund,
subscription, UsageRecord, LeaseGuard, and TaskFreeze paths.

**CHECK**:
- `payment_outbox` and `refund_outbox` pending counts and oldest age.
- `idempotency_keys_oldest_reserved_age_seconds`.
- Rust counters for billing settle, freeze release, and emit lag.

**MITIGATE**: Pause only the affected worker, preserve Redis streams and
PostgreSQL rows, and avoid manual balance edits until idempotency state is
known.

**FIX**: Repair the authoritative transaction or outbox processor. Do not
introduce a second settlement path outside PostgreSQL idempotency and Go dedup.

**VERIFY**: Replaying the affected request is idempotent, outbox rows reach a
terminal state, and wallet balance equals the sum of successful responses.

**OWNER**: Billing and wallet owner.

## WS-2 Worker Drainer And Tunnel Lifecycle

**IS**: Any invariant where ACK, close, XACK, or Redis/ClickHouse side effects
can occur before durable processing is complete.

**IS NOT**: Normal pending work while a worker is alive and its age is below the
claim/retry threshold.

**WHY**: WS-2 prevents ACK-before-side-effect loss and tunnel stream leaks.

**CHECK**:
- Redis PEL size for `usage:*`, `events:*`, and task streams.
- `gw_tunnel_active_streams` by node.
- Worker supervisor restart counters and sink flush errors.

**MITIGATE**: Stop new producers first, then let drainers flush. Do not close
channels underneath active senders.

**FIX**: Restore the ack-after-flush contract, XAUTOCLAIM reclaim path, or
tunnel close path.

**VERIFY**: PEL/backlog age drains, no duplicate ClickHouse rows remain after
dedup, and active tunnel streams return to baseline.

**OWNER**: Worker/drainer owner and gateway tunnel owner.

## WS-3 Bounded Memory

**IS**: Any unbounded buffer, map, cache, routing state, or Prometheus series
cardinality growth that threatens process or Prometheus stability.

**IS NOT**: Expected steady-state nonzero map/cache size below configured caps.

**WHY**: WS-3 makes memory pressure visible and bounded before it becomes an
outage.

**CHECK**:
- Routing size/cap metrics.
- Buffer overflow/drop/reject counters.
- Prometheus scrape payload size and metric-label lint output.

**MITIGATE**: Reduce traffic to the affected path or temporarily raise a cap
with a rollback note.

**FIX**: Add or repair the bounded policy and sweeper for the specific map or
buffer. Do not add hash buckets for high-cardinality identifiers.

**VERIFY**: Size/cap ratio stays below 80 percent, scrape payload stays under
the WS-10 threshold, and no banned metric label is present.

**OWNER**: Runtime memory owner and SRE.

## WS-4 Panic Supervisor

**IS**: Any panic, unobserved long-running goroutine/task, or worker health state
that can silently stop background work.

**IS NOT**: A one-shot task that is explicitly declared as non-restarting and
has a bounded lifecycle.

**WHY**: WS-4 keeps panics observable and readiness honest.

**CHECK**:
- `*_panic_recovered_total`.
- `*_supervisor_restart_total`.
- `/readyz` and worker unhealthy gauges.

**MITIGATE**: Drain an unstable pod if the same worker repeatedly panics.

**FIX**: Move the worker under the Go/Rust supervisor API or classify it as a
bounded one-shot with a test.

**VERIFY**: Panic injection tests recover as expected and readiness reflects the
unhealthy worker state.

**OWNER**: Platform runtime owner.

## WS-5 Fail-Closed Guards

**IS**: Any auth, license, API-key, cache invalidation, JWT, HMAC, XFF, or
management-token failure that could continue as allow/open.

**IS NOT**: A deliberate warning-only rollout window such as license enforcement
before the D14 enforce date.

**WHY**: WS-5 removes fail-open security behavior and keeps external errors
non-oracle.

**CHECK**:
- License mode and `license_block_total{mode}`.
- JWT/HMAC/mgmt-token failure counters and server logs.
- Cache invalidation DLQ and API-key mutation results.

**MITIGATE**: For enforcement spikes, roll back to the documented warn mode; for
credential compromise, revoke and invalidate caches before re-enabling traffic.

**FIX**: Restore fail-closed storage/cache behavior and generic public error
codes while preserving detailed server-side logs.

**VERIFY**: Forbidden scenarios return 401/402/503 as specified, cache mutation
failures return 503, and no public state oracle remains.

**OWNER**: Security and auth owner.

## WS-6 HTTP Response Integrity

**IS**: Any path where parse failure, retry, cancellation, or cross-protocol
rendering can leak malformed upstream bytes or produce mismatched billing state.

**IS NOT**: A same-protocol passthrough response that has not entered the
cross-render path.

**WHY**: WS-6 guarantees client-visible response bytes match the protocol and
billing/error state.

**CHECK**:
- `gw_upstream_protocol_decode_error_total`.
- Response retry reset counters.
- Archive overflow/drop counters for response surfaces.

**MITIGATE**: Disable the affected vendor/protocol pair or route traffic to a
same-protocol fallback.

**FIX**: Repair codec parse, reset, or error fanout in the Rust response path.

**VERIFY**: Cross-protocol parse-failure tests return 502 with no raw upstream
body, and billing emits the expected zero/terminated state.

**OWNER**: Gateway codec owner.

## WS-7 Concurrency And Numeric Safety

**IS**: Any timeout, lock, singleflight, vendor-factor, cooldown, freeze amount,
or weighted routing behavior that can overflow, panic, stall, or avalanche.

**IS NOT**: A bounded retry/cooldown within the configured clamp.

**WHY**: WS-7 removes hidden concurrency and numeric hazards from hot paths.

**CHECK**:
- Cache singleflight timeout/orphan/type-mismatch metrics.
- API rate-limit map size.
- `gw_health_cooldown_clamp_total`.
- Freeze overflow and vendor-factor tests.

**MITIGATE**: Reduce affected traffic or temporarily disable the risky vendor
factor/routing strategy.

**FIX**: Use bounded contexts, type-tagged keys, i128/u32 micro-unit math, and
health transition observers as specified by WS-7.

**VERIFY**: Focused Go/Rust tests pass and static scans show no money `as i64`
casts in touched paths.

**OWNER**: Runtime and gateway routing owner.

## WS-8 Usage Schema Contract

**IS**: Any Rust to Go usage schema, provenance, `_pv`, request-id, or
attempt-index mismatch that can cause silent billing data loss.

**IS NOT**: Unknown additive fields whose `_pv` is within the supported window.

**WHY**: WS-8 keeps mixed-version deployments from silently dropping or
mis-deduplicating usage records.

**CHECK**:
- Rustpact `_pv` values and Go min-supported version.
- UsageRecord dedup keys include `(request_id, attempt_index)`.
- Contract sync diffs between Rust `docs/go-contracts` and Go `docs/rust-contract`.

**MITIGATE**: Halt deploy ordering if the reader cannot consume the writer's
version.

**FIX**: Update the contract, `_pv`, Go parser, and deploy-order lint together.

**VERIFY**: Mixed-version contract tests pass and contract sync has no
unreviewed remove/rename diff.

**OWNER**: Cross-repo contract owner.

## WS-9 Query Performance

**IS**: Any query path that can become operationally invisible or overload a
database during incident load.

**IS NOT**: A slow backfill or offline admin report outside request/incident
paths.

**WHY**: WS-9 prevents observability and admin workflows from failing during the
same incident they are needed to diagnose.

**CHECK**:
- Query latency metrics for billing/admin dashboards.
- Database slow-query logs for the matching endpoint.
- Worker/reconciler batch sizes and first-error abort counts.

**MITIGATE**: Disable the expensive dashboard/report or narrow the query window
while keeping billing settlement running.

**FIX**: Add indexes, materialized summaries, or pagination according to the
WS-9 task that owns the query.

**VERIFY**: The endpoint/query meets its latency threshold under representative
load and does not starve settlement workers.

**OWNER**: Go performance owner.
