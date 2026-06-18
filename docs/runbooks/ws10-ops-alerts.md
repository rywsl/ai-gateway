# WS-10 Ops Alerts Runbook

These alerts cover the WS-10 launch-gate operational invariants. Do not add
`request_id`, `user_id`, or key IDs to Prometheus labels while triaging. Use
structured logs or ClickHouse detail tables for per-request drill-down.

## SupervisedWorkerPanicRecovered

**IS**: A supervised Go or Rust worker recovered from at least one panic in the
last five minutes.

**IS NOT**: A process crash or customer-facing outage by itself; the supervisor
kept the process alive.

**WHY**: WS-4 requires panic recovery to be observable. A recovered panic can
otherwise leave a worker repeatedly restarting with no operator signal.

**CHECK**:
- PromQL: `sum by (job, name) (increase({__name__=~".*_panic_recovered_total"}[5m])) > 0`
- `kubectl -n alm logs deploy/<job> --since=10m | grep -E 'panic|recovered|supervisor'`
- Check `/readyz` for the affected pod and compare supervisor restart counters.

**MITIGATE**: If the same worker panics twice in 15 minutes, drain that pod from
traffic and keep logs for the owning team.

**FIX**: Follow the owning WS path for that worker. For Go workers start at
`shared/runtime`; for Rust workers start at `crates/gw-runtime`.

**VERIFY**: The panic counter stops increasing, `/readyz` remains healthy for
30 minutes, and the affected worker's restart counter is flat.

**OWNER**: Platform runtime owner, with escalation to the WS owner named by the
worker.

## IdempotencyKeyReservedTooLong

**IS**: The oldest reserved idempotency key has been reserved for more than five
minutes.

**IS NOT**: Growth in total `idempotency_keys` rows; committed rows are expected
to accumulate until retention removes them.

**WHY**: WS-1 moves wallet idempotency into the PostgreSQL transaction. A long
reserved age means a wallet transaction, outbox processor, or state transition
is stuck.

**CHECK**:
- PromQL: `max(idempotency_keys_oldest_reserved_age_seconds) > 300`
- `SELECT op, key, status, created_at FROM idempotency_keys ORDER BY created_at LIMIT 20;`
- Inspect wallet/payment/refund/subscription worker logs around the oldest row.

**MITIGATE**: Pause the affected outbox worker before manual repair. Do not
manually debit or credit until the idempotency state is understood.

**FIX**: Repair the stuck wallet or outbox transition and keep the same
idempotency key path; do not bypass the idempotency table.

**VERIFY**: `idempotency_keys_oldest_reserved_age_seconds` returns below 300,
the stuck row reaches a terminal state, and replaying the request returns the
stored response without a second balance mutation.

**OWNER**: Billing and wallet owner.

## RoutingMapHighWaterMark

**IS**: A bounded routing map is above 80 percent of its exposed capacity for 10
minutes.

**IS NOT**: A nonzero map size. Routing maps are expected to hold active health,
round-robin, and in-flight state.

**WHY**: WS-3 requires bounded maps to expose pressure before eviction changes
routing behavior.

**CHECK**:
- PromQL: `max by (name) (gw_routing_size{name=~".+"}) / max by (name) (gw_routing_cap{name=~".+"}) > 0.8`
- Inspect vendor-key churn, key-health transitions, and recent routing config
  pushes.
- Check whether a single map name is responsible or all maps are growing.

**MITIGATE**: Temporarily raise the affected cap only if customer traffic is at
risk, and record the change for rollback.

**FIX**: Fix the map-specific retention, sweep, or transition observer path.
Do not add unbounded labels or per-request map keys.

**VERIFY**: Size/cap ratio remains below 0.8 for 30 minutes under normal load,
and no routing eviction counter is increasing unexpectedly.

**OWNER**: Gateway routing owner.

## TunnelActiveStreamsLeak

**IS**: `gw_tunnel_active_streams` for a node has a sustained positive slope for
15 minutes.

**IS NOT**: A temporary traffic spike; this alert is about monotonic growth, not
absolute stream count.

**WHY**: WS-2 requires the tunnel close path and permit ownership to release
streams after request completion.

**CHECK**:
- PromQL: `deriv(gw_tunnel_active_streams[15m]) > 0`
- Compare active streams with file descriptor count on the same pod.
- Inspect tunnel client/server logs for missing close frames or reconnect churn.

**MITIGATE**: Drain the affected gateway node, capture logs, and restart after
traffic is removed.

**FIX**: Repair the tunnel close path, writer permit ownership, or server-side
JoinSet cancellation path.

**VERIFY**: Active stream count returns to baseline after restart and remains
flat under steady traffic for 30 minutes.

**OWNER**: Gateway tunnel owner.
