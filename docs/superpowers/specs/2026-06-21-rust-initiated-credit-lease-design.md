# Rust-Initiated Credit Lease Activation

## Context

Current credit-lease hard mode is Go-initiated. `LeaseScheduler` scans every
active wallet with positive `recharge_balance_micros + gift_balance_micros`,
computes a lease, and fans it out to every live Rust pod. This makes Go work
scale with all funded users, not active users.

The new model makes Rust publish recent user activity. Go uses that activity
signal to decide which users should receive renewed `protected_lease` messages.

## Goals

- A funded user's first normal LLM proxy request must not require a pre-existing
  Go-pushed lease.
- Rust must not synchronously call Go on the request hot path.
- Go must stop scanning all funded wallets in steady state.
- Active users must continue to refresh their liveness while they use either the
  lease path or the cold soft-defense path.
- Inactive users must age out automatically and stop receiving renewals.
- Balance remains authoritative in Go/Postgres; Rust may read an authoritative
  balance for admission fallback but must not compute monetary settlement cost.

## Non-Goals

- No HTTP or gRPC request from Rust to Go for lease creation.
- No new per-request Go dependency in Rust admission.
- No change to the existing `protected_lease` payload shape sent from Go to Rust.
- No change to Go settlement cost calculation.
- No Phase 1 active-registration behavior for task endpoints or Gemini Files
  endpoints. Those paths have different billing and short-circuit semantics.

## Design

### Activation Boundary

Rust writes or refreshes active-user registration after every successfully
admitted normal billable LLM proxy request when `[protection].enabled = true`.
This includes:

- Cold users with no local `LEASE_POOL` entry that pass the hard-cap and
  `RequestFreeze` path.
- Protected users whose `LeaseGuard::try_acquire` succeeds.
- Protected users whose `Expired`, `Insufficient`, or protected-entry race
  `NoLease` outcome is admitted by `OverflowGate`.

The boundary is request admission, not upstream vendor success. A user is active
once Rust admits the request. Upstream failure, response parsing failure, or
settlement failure must not be required for liveness registration.

Rust must not refresh registration for:

- Balance rejection at step 7.
- Daily quota, API key, RPM, hard-cap, or freeze construction rejection.
- Task `Submit`, `Load`, `Watch`, or `Cancel`.
- Gemini Files endpoints.

### Balance Admission Source

Rust keeps the existing step 7 balance gate, but the gate must support a cold
counter fallback for normal LLM proxy requests:

1. Read the existing Redis counter snapshot.
2. If the Redis counter snapshot is positive, continue as today.
3. If the Redis counter snapshot is zero or missing, perform a read-only
   authoritative balance fallback using the existing Rust `OverflowGate`
   Redis balance cache and direct Postgres fallback, or an equivalent read-only
   gateway balance source.
4. If the fallback balance is positive, admit the request through the normal
   cold soft-defense path and enqueue active registration after admission.
5. If the fallback balance is zero, negative, unavailable, or cannot be trusted,
   return the existing 402 response and do not register the user.

This preserves the goal that a funded user can activate the lease flow without a
pre-pushed lease, while keeping Go/Postgres authoritative for funds.

### Redis Contract

Rust writes active-user registration through one atomic Redis operation. The
operation must either add the set membership and refresh the liveness marker
together, or fail and be retried by the worker.

| Key | Type | Writer | Reader | TTL | Meaning |
|---|---|---|---|---|---|
| `credit:active_users` | Set | Rust | Go | set-level fallback only | Candidate active users |
| `credit:active_user:<uid>` | String | Rust | Go | yes | Per-user liveness marker |
| `credit:lease_renew_at` | Sorted set | Go | Go | no | Per-user next renewal timestamp in ms |

Default Rust protection config:

- `active_registration_ttl_secs = 300`
- `active_registration_refresh_min_interval_secs = 30`
- `active_registration_queue_capacity = 8192`

The worker writes this registration atomically, using Lua or `MULTI/EXEC` in the
current single Redis/Sentinel topology:

```text
SADD credit:active_users <uid>
SET credit:active_user:<uid> "<last_seen_ms>|<pod_id>" EX <ttl_secs>
EXPIRE credit:active_users <ttl_secs>
```

The liveness marker key is authoritative. The set may contain stale members; Go
must remove members whose marker is absent. The marker value is opaque to Go for
renewal decisions, but must include `last_seen_ms` and `pod_id` for debugging.

If this Redis deployment moves to Cluster mode, the active keys must be migrated
to a same-slot hash-tagged contract before using Lua or multi-key transactions.

### Rust Admission Flow

Rust adds an `ActiveRegistrationHandle` that is wired at gateway boot and passed
to the request filter. Request code only calls a non-blocking
`try_enqueue_activity(uid, admitted_at_ms, pod_id)` method:

- No Redis I/O on the request hot path.
- No `.await` on the request hot path.
- No per-request `tokio::spawn`.
- Bounded in-process queue.
- Local per-user throttle using
  `active_registration_refresh_min_interval_secs`.

Enqueue failure is observable but non-fatal because the request was already
admitted by existing balance, lease, overflow, or freeze controls.

Cold and lease terms are distinct:

- True cold user or no local `LEASE_POOL` entry: run hard caps and
  `RequestFreeze`; after successful freeze admission, enqueue active
  registration.
- Existing protected entry: run the current `LeaseGuard` acquire path; after
  success, enqueue active registration.
- Protected-entry race `NoLease`: continue to `OverflowGate`; if overflow
  admission succeeds, enqueue active registration.
- `Expired` or `Insufficient`: continue to `OverflowGate`; if overflow admission
  succeeds, enqueue active registration.

Rust sweeps local `LEASE_POOL` entries when:

```text
now_ms > expires_at_ms + lease_grace_ms + active_registration_ttl_secs * 1000
```

`active_registration_ttl_secs` and `lease_grace_ms` must be available to the
ledger sweep task. This replaces the current protected-entry no-op sweep
invariant.

### Go Renewal Flow

Go `LeaseScheduler` supports three selection modes:

- `full_scan`: existing funded-wallet scan. Used only for rollback.
- `dual`: union of funded-wallet scan and active-registration scan. Used during
  rollout warmup.
- `active_only`: active-registration scan only. This is the steady state.

`active_only` and `dual` active scans work as follows:

1. The scheduler runs only while holding the credit-lease producer leader lock.
   Without the lock, the tick exits without pushing leases.
2. Keep an `SSCAN` cursor for `credit:active_users` in scheduler state and scan
   at most `active_scan_batch_size` users per tick. Default batch size is 500.
3. Pipeline liveness checks for `credit:active_user:<uid>`.
4. Remove stale, malformed, or invalid uid entries with `SREM` and `ZREM`.
5. Batch-read wallet rows from Postgres with a read-only query equivalent to:

   ```sql
   SELECT user_id, recharge_balance_micros + gift_balance_micros AS balance_micro
   FROM wallets
   WHERE status = 'active'
     AND user_id = ANY($1)
   ```

   The scheduler must not call wallet APIs that create missing wallets.
6. Missing wallet rows, inactive wallets, and non-positive balances are removed
   from `credit:active_users` and `credit:lease_renew_at`.
7. For live users with positive balance, read
   `ZSCORE credit:lease_renew_at <uid>`. Missing score or score `<= now_ms`
   means the user is due.
8. For due users, call existing `PushLease`.
9. After successful push, write
   `ZADD credit:lease_renew_at <next_renew_at_ms> <uid>`, where
   `next_renew_at_ms = now_ms + lease_ttl_max_ms / 2`.
10. Push failures leave the previous due score unchanged so a later tick retries.

Existing live-pod fanout remains unchanged: Go still sends `protected_lease` to
`events:counter:pod:<pod_id>` after filtering `gw:active_pods` through
`gw:active_pod:<pod_id>`.

### Leader and Deployment Constraints

The credit-lease producer must have a single active leader. The implementation
may use an existing runtime leader primitive or a Redis `SET NX PX` lease, but
two Go replicas must not execute `LeaseScheduler.Tick` concurrently.

If leader state is unavailable, the scheduler must skip the tick and emit a
metric. It must not fall back to unsafe multi-producer mode.

## Failure Semantics

| Failure | Behavior |
|---|---|
| Rust active registration throttled | Request proceeds; no Redis write is enqueued |
| Rust registration worker queue full | Request proceeds; metric increments; user may stay on cold or overflow path until a later admitted request refreshes registration |
| Rust registration worker closed during shutdown | Request proceeds if admission already succeeded; metric increments |
| Rust Redis registration fails | Worker retries with bounded retry policy; if exhausted, metric increments |
| Rust cold balance fallback fails | Request returns existing 402 balance rejection; no active registration |
| Go leader lock unavailable | Tick skips lease push; metric increments |
| Go active-user scan fails | Tick returns/logs error; no active-scan leases pushed in that tick |
| Go sees stale set member | `SREM` and `ZREM`, no lease push |
| Go sees malformed uid | `SREM` and `ZREM`, no lease push |
| Go sees missing, inactive, or zero-balance wallet | `SREM` and `ZREM`, no lease push |
| Go cannot read wallet balance | Skip current tick for affected batch and surface scheduler error/metric |
| Go `PushLease` fails | Leave due score unchanged so a later tick retries |

## Metrics and Alerts

Rust metrics:

- `gw_credit_active_register_enqueue_total{result=queued|throttled|full|closed|disabled}`
- `gw_credit_active_register_redis_total{result=success|retry_exhausted}`
- `gw_credit_active_register_queue_depth`
- `gw_credit_lease_sweep_removed_total`
- `gw_credit_balance_fallback_total{result=positive|zero|error}`

Go metrics:

- `credit_lease_active_scan_total{result=scanned|live|stale|malformed|missing_wallet|zero_balance|balance_error|pushed|skipped_not_due}`
- `credit_lease_active_users` gauge for live candidates seen in the current tick
- `credit_lease_active_scan_duration_seconds`
- `credit_lease_active_scan_cursor_wrap_total`
- `credit_lease_leader_total{result=acquired|lost|unavailable}`

Existing `credit_lease_xadd_fanout_total` remains. `credit_lease_due_users`
must keep a clear meaning: users due for renewal in the current tick, not total
active set size.

Alerts must cover:

- Rust registration queue full or sustained high queue depth.
- Rust registration retry exhaustion.
- Go leader absent or duplicate producer detection.
- Go active scan errors.
- High stale or malformed active-set ratio.
- Active users nonzero while `credit_lease_xadd_fanout_total{result="success"}`
  is flat.
- Active-only mode enabled while old Rust pods without active-registration
  capability are still receiving traffic.

## Tests

Rust unit tests:

- Default config uses 300 s active TTL, 30 s refresh throttle, and 8192 queue
  capacity.
- No lease plus admitted normal LLM request enqueues active registration.
- Lease acquire success enqueues or throttles active registration.
- `Expired`, `Insufficient`, and protected-entry race `NoLease` enqueue after
  overflow admission succeeds.
- Zero balance rejection does not enqueue registration.
- Cold counter with positive authoritative fallback admits and enqueues.
- Cold counter with zero or failed authoritative fallback rejects and does not
  enqueue.
- Task and Gemini Files paths do not enqueue active registration.
- Queue-full and queue-closed paths are non-fatal and increment metrics.
- Worker writes set membership, marker `SET EX`, and set `EXPIRE` atomically.
- Lease sweep removes entries older than
  `expires_at + lease_grace + active_registration_ttl`.

Go unit tests:

- Scheduler scans only active registrations in `active_only` mode.
- `dual` mode preserves existing funded-wallet coverage plus active
  registrations.
- `full_scan` mode preserves existing funded-wallet behavior for rollback.
- Scheduler stores and resumes `SSCAN` cursor across ticks.
- Scheduler scans at most 500 active users per tick by default.
- Scheduler uses batched read-only wallet SQL and does not create missing
  wallets.
- Stale, malformed, missing-wallet, inactive-wallet, and zero-balance users are
  removed from both active set and renew zset.
- Live active user with positive balance and due score fans out existing
  `protected_lease`.
- Not-due users are skipped without fanout.
- Push failure leaves due score retryable.
- Leader unavailable skips tick without pushing.

Integration/e2e:

- First normal LLM request with authoritative positive balance succeeds without
  a pre-existing lease and creates active registration.
- Next scheduler tick pushes `protected_lease`.
- Later request uses Rust `LEASE_POOL` and refreshes active registration through
  the throttled path.
- Continuous requests for `2 * active_registration_ttl_secs` keep the liveness
  marker fresh and leases renewed.
- After requests stop and active TTL expires, Go stops renewal and removes stale
  membership.
- Mixed old and new Rust rollout stays in `dual` mode until all pods have the
  active-registration capability.
- Polluted active set with many stale or malformed members is cleaned within the
  scan budget without blocking valid users.
- Go restart or leader handoff does not fan out a full active-user burst because
  `credit:lease_renew_at` persists due state.

## Rollout

1. Deploy Rust with active-registration writes enabled and Go still in
   `full_scan`. Old Go ignores the new Redis keys.
2. Verify Rust metrics show successful registrations, low retry exhaustion, and
   no sustained queue pressure.
3. Deploy Go with `selection_mode=dual` and leader protection enabled.
4. Keep `dual` mode for at least:

   ```text
   active_registration_ttl_secs + lease_ttl_max_ms / 1000 + lease_grace_ms / 1000
   ```

   and until all live Rust pods advertise the active-registration capability or
   the deployment system otherwise guarantees no old Rust pods receive traffic.
5. Switch Go to `active_only`.

Rollback:

- Switch Go back to `dual` or `full_scan`.
- Rust may keep writing active registrations; old or rollback Go ignores them in
  `full_scan`.
- Active Redis keys naturally expire. `credit:lease_renew_at` stale members are
  cleaned by the scheduler when active entries are removed.

## Documentation Updates

Rust implementation must update `docs/spec/06-billing-ledger.md`,
`docs/workflow.md`, and `docs/go-contracts/main.md`.

Go implementation must update `docs/business/billing/business.md`,
`docs/business/budgetpolicy/business.md` if policy knobs are added, and
`shared/rustpact` docs/constants for the new Redis contract.
