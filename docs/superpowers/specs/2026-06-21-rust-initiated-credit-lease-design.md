# Rust-Initiated Credit Lease Activation

## Context

Current credit-lease hard mode is Go-initiated. `LeaseScheduler` scans every active wallet with positive `recharge_balance_micros + gift_balance_micros`, computes a lease, and fans it out to every live Rust pod. This makes Go work scale with all funded users, not active users.

The new model makes Rust create the first activation signal. Go only renews users that Rust has recently registered as active.

## Goals

- First request for a funded user must not require a pre-existing Go-pushed lease.
- Rust must not synchronously call Go on the request hot path.
- Go must stop scanning all funded wallets for credit-lease renewal.
- Inactive users must age out automatically and stop receiving renewals.
- Balance remains authoritative in Go/Postgres; Rust does not compute monetary cost.

## Non-Goals

- No HTTP or gRPC request from Rust to Go for lease creation.
- No new per-request Go dependency in Rust admission.
- No change to the existing `protected_lease` payload shape sent from Go to Rust.
- No change to Go settlement cost calculation.

## Design

### Redis Contract

Rust writes an active-user registration after a request is admitted without an existing lease:

| Key | Type | Writer | Reader | TTL | Meaning |
|---|---|---|---|---|---|
| `credit:active_users` | Set | Rust | Go | set-level fallback only | Candidate active users |
| `credit:active_user:<uid>` | String | Rust | Go | yes | Per-user liveness marker |

Default TTL and buffering are Rust protection config:

- `active_registration_ttl_secs = 300`
- `active_registration_queue_capacity = 8192`

Registration write:

```text
SADD credit:active_users <uid>
SET credit:active_user:<uid> 1 EX 300
EXPIRE credit:active_users 300
```

The per-user key is authoritative. The set can contain stale members; Go must remove members whose liveness key is absent.

### Rust Admission Flow

Rust keeps the existing step 7 balance gate. If `available <= 0`, it returns 402 and does not register the user.

When `[protection].enabled = true` and no local `LEASE_POOL` entry exists:

1. Continue through the existing hard-cap and `RequestFreeze` soft-defense path.
2. If admission succeeds, enqueue `effective_user_id` into a bounded in-process registration worker.
3. The worker performs the Redis registration asynchronously.
4. Worker enqueue failure is observable but non-fatal; the request is already admitted by existing balance/freeze controls.

This uses admission success as the activation boundary. Upstream vendor success is not required, because the user is active once Rust admits the request.

When a lease exists, Rust continues to use the existing credit-lease acquire path. `Expired`, `Insufficient`, and `NoLease` continue to route through overflow admission.

Rust also sweeps local `LEASE_POOL` entries when `now_ms > expires_at_ms + lease_grace_ms + active_registration_ttl_secs * 1000`. This prevents inactive users from remaining in memory indefinitely after Go stops renewal.

### Go Renewal Flow

Go `LeaseScheduler` stops selecting from all positive wallets. Each tick:

1. Scan `credit:active_users` in bounded batches, default batch size 500.
2. For each uid, check `EXISTS credit:active_user:<uid>`.
3. If absent, `SREM credit:active_users <uid>` and skip.
4. For live uids, read the wallet balance from Postgres.
5. If balance is `<= 0`, `SREM credit:active_users <uid>` and skip.
6. If balance is positive and the user is due, call existing `PushLease`.

Existing live-pod fanout remains unchanged: Go still sends `protected_lease` to `events:counter:pod:<pod_id>` after filtering `gw:active_pods` through `gw:active_pod:<pod_id>`.

## Failure Semantics

| Failure | Behavior |
|---|---|
| Rust registration worker queue full | Request proceeds; metric increments; user may stay on cold path until a later admitted request registers |
| Rust Redis registration fails | Worker retries with bounded retry policy; if exhausted, metric increments |
| Go active-user scan fails | Tick returns/logs error; no leases pushed in that tick |
| Go sees stale set member | `SREM`, no lease push |
| Go sees active uid with zero balance | `SREM`, no lease push |
| Go cannot read wallet balance | Skip current tick for that uid and surface scheduler error/metric |

## Metrics

Rust:

- `gw_credit_active_register_enqueue_total{result=queued|full|disabled}`
- `gw_credit_active_register_redis_total{result=success|retry_exhausted}`
- `gw_credit_active_register_queue_depth`
- `gw_credit_lease_sweep_removed_total`

Go:

- `credit_lease_active_scan_total{result=live|stale|zero_balance|balance_error}`
- `credit_lease_active_users` gauge for currently live candidates in a tick

Existing `credit_lease_xadd_fanout_total` and `credit_lease_due_users` remain.

## Tests

Rust unit tests:

- No lease + admitted request enqueues active registration.
- Zero balance rejection does not enqueue registration.
- Worker writes `SADD`, per-user `SET EX`, and set `EXPIRE`.
- Default config uses 300 s active TTL and 8192 queue capacity.
- Queue-full path is non-fatal and increments metrics.
- Lease sweep removes entries older than `expires_at + lease_grace + active_registration_ttl`.

Go unit tests:

- Scheduler scans only active registrations, not all positive wallets.
- Scheduler scans in bounded 500-user batches.
- Stale active set members are removed.
- Zero-balance active users are removed and not renewed.
- Live active user with positive balance still fans out existing `protected_lease`.

Integration/e2e:

- First request with positive balance succeeds without a pre-existing lease and creates active registration.
- Next scheduler tick pushes `protected_lease`.
- Later request uses Rust `LEASE_POOL`.
- After active TTL expiry, Go stops renewal and stale membership is removed.

## Rollout

Deploy Rust first. New Rust writes active registration while old Go ignores it.

Deploy Go second. New Go switches scheduler selection to active registrations. If emergency rollback is needed, the Go scheduler can retain a temporary config flag to fall back to full funded-wallet scan.

## Documentation Updates

Rust implementation must update `docs/spec/06-billing-ledger.md`, `docs/workflow.md`, and `docs/go-contracts/main.md`.

Go implementation must update `docs/business/billing/business.md`, `docs/business/budgetpolicy/business.md` if policy knobs are added, and `shared/rustpact` docs/constants for the new Redis contract.
