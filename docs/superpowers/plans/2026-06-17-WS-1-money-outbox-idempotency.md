# WS-1 Sub-Plan — Commit-then-side-effect (Money / Idempotency / Outbox)

> **Parent**: `docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-1 + `2026-06-17-prelaunch-eradication-master-plan-v2.md` §4 REV-FINAL-M1..M11.
> **Audience**: an executing agent who **does not** know Rust or Go deeply. Each task is self-contained.
> **Authoritativeness**: every section names the file it must edit; the **SOURCE-CHECK** block of each task quotes the actual line that the executor must Read before touching anything. v1/v2 sonnet output is HYPOTHESIS — when source disagrees, source wins and the deviation is recorded inline.
> **Format**: 8 sections per task (1 IS · 2 NOT · 3 WHY · 4 DO · 5 DON'T · 6 HOW · 7 SOURCE-CHECK · 8 VERIFY).

---

## 0. Reading Discipline (do this FIRST for every task)

1. Open the file at every cited path with `Read`.
2. Compare the lines you read to the SOURCE-CHECK excerpt in this plan. If they differ:
   - **Trust source**, not this plan.
   - Add a `### Deviation` note under the task, naming the diff (e.g. "lease.rs released field is at line 121, not 117 as plan says").
   - Continue with the verified line numbers.
3. For each candidate fix you ship, write 1 sentence "Alternative considered" + 1 sentence "why this one" inline in the PR description (mini-ADR; see Task T01 for an example).
4. Sonnet hallucination has been proven; treat every `REV-FINAL-*` description below as a starting hypothesis.
5. When a task body references a v2 INVESTIGATE-N, do NOT assume a conclusion — record it as "open" in the PR description and surface it to the team lead.

---

## 1. Source Files Verified (line ranges Read by plan author)

| Path | Lines Read | Confirms / Contradicts |
|---|---|---|
| `AILinkMax-Go/modules/wallet/business/idem.go` | 1-30 | **Confirms M1** — only `Reserve` exists (line 21-30); on `key==""` returns `(true, nil)` (line 22-24); on Redis ok returns `(ok, nil)`. **No Release method**. |
| `AILinkMax-Go/modules/wallet/business/service.go` | 1-919 | **Confirms M1** — `Deduct` line 243 calls `Reserve`; on `!ok` returns synthetic `DeductionResult{BalanceAfter: walletToBalance(w)}` with zero deduction (line 246-251). Same pattern at `Credit:403`, `RecordUsage:354`, `Refund:498`, `adminDebit:568`. **Confirms M4** — `AdminAdjust` (line 851-897) passes empty `idemKey` to both `credit` and `adminDebit` (lines 866, 876). |
| `AILinkMax-Go/modules/wallet/api/handlers_admin.go` | 1-242 | **Confirms M4** — `adjust` handler (line 190) calls `h.svc.AdminAdjust(..., "")` at line 202 (no client `Idempotency-Key` header read). |
| `AILinkMax-Go/modules/payment/business/webhook.go` | 1-176 | **Confirms M2** — `Receive` (line 49) on `n.Direction == "deposit"` + completed/succeeded calls `s.orders.Credit(...)` (line 100) then `markProcessed(cb.ID, cerr)` (line 105). The Credit error is swallowed; handler returns `(cb, nil)` (line 115). |
| `AILinkMax-Go/modules/payment/business/orders.go` | 1-442 | **Confirms M2** — `Credit` (line 160) commits tx with `succeeded` state (line 161) then **outside tx** calls `s.wallet.Credit` (line 174); on failure returns `(row, false, err)`. State already in DB. |
| `AILinkMax-Go/modules/payment/business/refunds.go` | 1-473 | **Confirms M3** — `Complete` (line 130) commits refund `succeeded` via `transition` (line 131) **then** calls `s.wallet.Refund` (line 145) outside tx; the source-side comment at lines 161-167 acknowledges "状态已 succeeded 但钱包未扣（under-refund 风险）". **Confirms G2** — `Create` (line 54) reads balance via `wallet.GetBalance` (line 58) and compares to `bal.RechargeMicros` (line 62) but **does not** query pending refund requests for sum-deduplication and **does not** take any row lock on wallet. |
| `AILinkMax-Go/modules/billing/business/quota.go` | 1-91 | **Confirms G1** — `Check` runs a single `SELECT us.id, ... FROM user_subscriptions ... WHERE us.user_id=$1 AND us.status='active' ORDER BY ... LIMIT 1` (lines 73-78) with NO `FOR UPDATE` and `decideQuotaSource` is a pure function returning a struct. There is no Apply step bound in the same tx. |
| `AILinkMax-Go/modules/subscription/business/subscription.go` | 1-255 | **Confirms G5** — `Cancel` (line 100) executes `UPDATE user_subscriptions SET status='canceled', ... WHERE id=$1` (lines 124-127) **without** `AND status='active'`. **Confirms G7** — `Upgrade` (line 145) runs UPDATE-to-expired (lines 163-167) then calls `s.orders.Purchase(ctx, ...)` (line 169) in a **separate** db connection (no tx propagated). |
| `AILinkMax-Go/modules/subscription/business/cycle.go` | 1-149 | **Confirms G6** — `RollIfDue` (line 34) reads `sub` via `c.subs.Get(ctx, subID)` (line 35) before `c.pool.Begin` (line 65); the branch test at line 71 still uses `sub.CancelAtPeriodEnd` / `sub.EndsAt` from the pre-lock copy. |
| `AILinkMax-Go/modules/wallet/business/counter_relay.go` | 1-159 | **Confirms G3 (partially)** — `Set(ctx, key, row.BalanceMicro, 0)` at line 135 uses TTL `0` (no expiry). `ListUnprocessed` (line 50) is `q.ListUnprocessedWalletCounterOutbox(ctx, limit)` (sqlc query — ordering must be verified in `modules/wallet/model/queries/*.sql`). G3 reordering symptom (chunked commit-time vs serial id) is unproven from this file alone; treat that part as candidate. |
| `AILinkMax-Go/modules/mgmttoken/model/blacklist.go` | 1-42 | **Confirms G10** — `Revoke` (line 33) calls `_ = b.r.Set(...).Err()` at line 40 and is fire-and-forget (no error return). |
| `AILinkMax-Go/modules/billing/business/dedup.go` | 1-583 | **Confirms C2** — `provenanceRank` map (lines 18-25) has only `upstream_final / upstream_partial / estimated / crash_fallback / unknown / ""`. **No `cli_extracted`**. **Confirms C4** — `dedupKey` (line 56) returns `"settled:" + requestID`; no `attempt_index` dimension. **Confirms C7 risk** — Go side still uses Redis-only dedup keys; no PG-backed idempotency. |
| `ailinkmax-gateway/crates/gw-ledger/src/lease.rs` | 1-471 | **Confirms M3/Billing-1/5/6** — `LeaseGuard` (line 117) fields = `{uid, reserved, coef_version, released, state}`. **No `settled` field**. `settle_actual` (line 199-206) is **not** idempotent — it just does `fetch_add(refund)` and an `actual >= reserved` short-circuit but **does not** mark itself done. `release` (line 211-215) only does CAS on `released`, no refund. `Drop` (line 218-222) only calls `release`. Confirms REV-FINAL-M3 architecturally; **executor must still grep `settle_actual` for every caller before choosing impl candidate** (see Task T13). |
| `ailinkmax-gateway/crates/gw-ledger/src/freeze.rs` | 1-217 | **Confirms R5** — `TaskFreeze::release` (line 183-209) CAS-sets `released=true` at line 186 **then** `tokio::spawn`s the releaser (line 198) and only `tracing::warn!`s on err (line 200-206). The CAS happens **before** the async task runs. **Confirms R9** — task_id is just `String` (line 147) passed in via `TaskFreeze::new`; no PG-backed pending table. |
| `ailinkmax-gateway/crates/gw-ledger/src/inflight_reconciler.rs` | 1-163 | **Confirms R6/Billing-3** — `run` (line 28) emits via callback (line 46), then `pool.del::<(), _>(&[key_str.as_str()]).await?` at line 54 — the `?` propagates DEL error to caller, leaving the XADD already-applied. **Also**: `build_fallback_record` (line 76) does **not** read the `state` field of the hash — only `prompt_estimate_tokens / ts_sent_ms / vendor_key_id / freeze_amount` are extracted. So a `state=settled` residue is re-emitted as CrashFallback. |
| `ailinkmax-gateway/crates/gw-ledger/src/flusher.rs` | 190-260, full count 1121 | **Confirms Billing-8** — `flush()` inside `AssertUnwindSafe(...).catch_unwind().await` at line 208; on `Err(_panic)` the `taken: Vec<LedgerItem>` (line 205) is **dropped** and the comment at line 213 reads "channel kept open, batch lost". |
| `ailinkmax-gateway/crates/gw-ledger/src/backlog.rs` | partial via grep | **Confirms Billing-4** — `let _ = tokio::fs::remove_file(path).await` at line 252; `let _ = tokio::fs::rename(&p, &dest).await` at line 229. Both swallow errors. Each envelope is re-pushed if removal fails after backlog.push succeeded. |
| `ailinkmax-gateway/crates/gw-proxy/src/filters/logging.rs` | grepped 100-180 | **Confirms Billing-1/R7** — line 150 `match gw_ledger::actual_usage_credits(...)`; line 159 `guard.settle_actual(actual);`; line 169 `"credit-lease actual_usage_credits failed; settle skipped"`; line 176 `g.release();` (no settle_actual(0) on Err). Same release-only path also at line 114 and line 146 for CLI early-return. |
| `ailinkmax-gateway/crates/gw-proxy/src/filters/task.rs` | grepped 230-240 | **Confirms R9** — line 235 calls `.release_task_freeze(request.user_id, &task_id, frozen_micro)` direct over `contract`; no PG persistence of `(user_id, task_id, frozen_micro)` is performed at submit time. |

**Falsified from sonnet output**: none in WS-1 scope at this time. INVESTIGATE items remain open (see §6).

---

## 2. Coverage Matrix (Task → Finding ID)

| Task ID | Finding ID(s) | Note |
|---|---|---|
| T01 | M1 (root cause: idem table) | Creates `idempotency_keys` migration + Repo. |
| T02 | M1 (interface) | New `Idem` interface signature (Commit-only). |
| T03 | M1 / wallet.Deduct | Switch `Deduct` to in-tx idem.Commit. |
| T04 | M1 / wallet.Credit | Switch `Credit` to in-tx idem.Commit. |
| T05 | M1 / wallet.Refund | Switch `Refund` to in-tx idem.Commit. |
| T06 | M1 / wallet.adminDebit + RecordUsage | Same shape. |
| T07 | M4 / REV-FINAL-M11 | AdminAdjust: namespaced server-side key. |
| T08 | M2 / G4 / payment_outbox / REV-FINAL-M1 / REV-FINAL-M5 | payment_outbox table + processor + webhook rewrite. |
| T09 | M3 / G2 / refund_outbox / REV-FINAL-M4 | refund_outbox + Create double-check with advisory lock. |
| T10 | G1 | Subscription quota CheckAndApply (single tx + FOR UPDATE + CHECK). |
| T11 | G5 | Cancel + RowsAffected guard. |
| T12 | G6 | RollIfDue re-read inside tx. |
| T13 | G7 / REV-FINAL-M10 | subscription_upgrade_outbox (cross-module via outbox). |
| T14 | G10 | mgmttoken Blacklist.Revoke returns error. |
| T15 | G3 / REV-004a | wallet_counter_outbox commit_timestamp + relay ordering. |
| T16 | Billing-1 / Billing-5 / Billing-6 / R7 (lease side) / REV-FINAL-M3 | Rust LeaseGuard refund-on-Drop + logging.rs Err / CLI paths. |
| T17 | R5 / REV-007a | TaskFreeze sync-then-async release + DLQ + Redis retry. |
| T18 | R9 | task_freeze_pending PG table + sweeper. |
| T19 | Billing-2 / Billing-3 / R6 / REV-FINAL-M7 / REV-FINAL-M8 | Atomic Lua `(XADD ; HSET state=settled ; DEL inflight)` + state-aware reconciler. |
| T20 | R7 / WS-6 cross-reference | parse_response Err HTTP path = 502 (lease portion); WS-6 owns response_filter side. |
| T21 | Billing-4 / REV-FINAL-M9 | replay_file per-envelope SETNX + bounded spill semaphore. |
| T22 | Billing-8 / REV-FINAL-M9 | catch_unwind spill-to-disk reroute. |
| T23 | C2 | provenanceRank add `cli_extracted` (Go). |
| T24 | C4 / REV-FINAL-H8.7 / REV-FINAL-H8.9 | dedupKey + attempt_index; Rust-first, Go reads both shapes. |
| T25 | C3 | Terminated semantic — credits_consumed=Some(0)/cost_micros=0 plumbed. |
| T26 | C7 | wallet:idem dual-write migration script + cutover. |
| T_BACKFILL_PAYMENT | meta-§H (REV-009) | One-shot backfill for v1-era abandoned payment rows. |
| T_TEST | DoD chaos suite for WS-1 | New tests under `tests/e2e/chaos/ws1/` (depends on WS-13 harness). |

---

## 3. Task Specifications

> The 8-section format is enforced. Code blocks are **literal**. If you find yourself writing "as needed" or "appropriate", STOP and read the source again.

---

### TASK T01 — Create `idempotency_keys` PG table + repo

#### 1. 【是什么 What it IS】
- A goose migration file `migrations/00035_idempotency_keys.sql` (Go monorepo).
- A new repo module `modules/wallet/model/idem_repo.go` exposing 1 method `CommitIdem(ctx, tx, op, key, requestHash, responseJSON) (didWrite bool, prior []byte, err error)`.
- The migration creates one table partitioned by month on `created_at` (per REV-FINAL-M6).

#### 2. 【不是什么 What it is NOT】
- NOT a Redis change — the Redis `wallet:idem:*` SETNX path stays alive for one release window (dual-write per T26 / C7).
- NOT a rewrite of `service.go`. The service-side switch happens in T03-T06.
- NOT a new module — this lives under `modules/wallet/model` (per CLAUDE.md model-layer rule); it does NOT import anything from `modules/payment` etc.

#### 3. 【为什么 Why this bug exists】
- Root: M1 + v2 REV-FINAL-M2. `Idem.Reserve` SETNX + `repo.InTx` are independent; tx rollback leaves the Redis key alive (24 h TTL), and the next retry takes the `(false, nil)` short-circuit path and returns a synthetic success with **zero** deduction.
- The architectural cure is to put idempotency state INTO the wallet tx so commit/rollback are jointly atomic.
- Audit refs: original Go audit §3.1 [M1]; v2 §4 WS-1 REV-FINAL-M1 & REV-FINAL-M2.

#### 4. 【做什么 DO】
1. Create `migrations/00035_idempotency_keys.sql` per `HOW`.
2. Add **down** script `migrations/00035_idempotency_keys.down.sql` (drop partitioned table; this drops history — annotate the irreversible cost in the migration header).
3. Add a sqlc query file `modules/wallet/model/queries/idempotency_keys.sql` containing the **single** `INSERT ... ON CONFLICT DO UPDATE ... RETURNING (xmax = 0), response_jsonb` statement.
4. Implement `modules/wallet/model/idem_repo.go` that calls the sqlc-generated row using a passed-in `pgx.Tx` (not from `pool.Begin`).
5. Add unit tests under `modules/wallet/model/idem_repo_test.go` using `pgxmock` covering: first-write returns `(didWrite=true, prior=nil)`; second-write returns `(didWrite=false, prior=<bytes>)`; concurrent first-and-second resolve deterministically.

#### 5. 【不做什么 DON'T】
- Do **NOT** use `ON CONFLICT DO NOTHING` — under READ COMMITTED the conflict row is invisible to the second caller; the second SQL would return zero rows. REV-FINAL-M2 confirms this.
- Do **NOT** index on `(key)` alone — PK on `(op, key)` is the uniqueness guarantee; `op` partitions the namespace.
- Do **NOT** add columns the consumer doesn't need (e.g. `actor_id`); the table stays append-only with one purpose.
- Do **NOT** make `response_jsonb` nullable — empty payloads must be `'{}'::jsonb`.
- Do **NOT** add a foreign key to wallets/users — the table must remain operable while either side is migrating.

#### 6. 【怎么做 HOW (具体)】

`migrations/00035_idempotency_keys.sql`:

```sql
-- +goose Up
-- +goose StatementBegin
-- Partitioned by month on created_at. 90 d retention (drop oldest partition by a
-- weekly cron — see scripts/idempotency-keys-prune.sh, owned by WS-1 T01).
CREATE TABLE idempotency_keys (
    op             text         NOT NULL,
    key            text         NOT NULL,
    request_hash   text         NOT NULL,
    response_jsonb jsonb        NOT NULL DEFAULT '{}'::jsonb,
    created_at     timestamptz  NOT NULL DEFAULT now(),
    PRIMARY KEY (op, key, created_at)
) PARTITION BY RANGE (created_at);

-- Bootstrap partitions for current + next 2 months. Cron job extends monthly.
CREATE TABLE idempotency_keys_2026_06 PARTITION OF idempotency_keys
    FOR VALUES FROM ('2026-06-01') TO ('2026-07-01');
CREATE TABLE idempotency_keys_2026_07 PARTITION OF idempotency_keys
    FOR VALUES FROM ('2026-07-01') TO ('2026-08-01');
CREATE TABLE idempotency_keys_2026_08 PARTITION OF idempotency_keys
    FOR VALUES FROM ('2026-08-01') TO ('2026-09-01');

CREATE INDEX idempotency_keys_created_at_idx ON idempotency_keys (created_at);
-- +goose StatementEnd

-- +goose Down
-- +goose StatementBegin
DROP TABLE IF EXISTS idempotency_keys;
-- +goose StatementEnd
```

`modules/wallet/model/queries/idempotency_keys.sql`:

```sql
-- name: CommitIdem :one
INSERT INTO idempotency_keys (op, key, request_hash, response_jsonb, created_at)
VALUES ($1, $2, $3, $4, now())
ON CONFLICT (op, key, created_at) DO UPDATE
    SET request_hash = idempotency_keys.request_hash
RETURNING
    (xmax = 0)         AS did_write,
    response_jsonb     AS prior_response;
```

(Note: PK includes `created_at` because the table is RANGE-partitioned; PG requires the partition key in PK. The `(op, key)` uniqueness is then enforced by a partial unique index per partition — automatically created by `CREATE TABLE ... PARTITION OF`. Verify on staging that two concurrent inserts of the same `(op, key)` produce exactly one row by reading PG docs §5.11.5.)

`modules/wallet/model/idem_repo.go`:

```go
package model

import (
    "context"
    "encoding/json"

    "github.com/jackc/pgx/v5"

    walletmodel "github.com/cnxx-tech/ailinkmax-go/modules/wallet/model/sqlcgen"
)

// CommitIdem inserts an idempotency row inside the caller's tx. didWrite=true
// means this caller owns the row and should perform its business logic;
// didWrite=false means prior holds the historical response_jsonb verbatim.
func CommitIdem(
    ctx context.Context,
    tx pgx.Tx,
    op, key, requestHash string,
    response any,
) (didWrite bool, prior []byte, err error) {
    body, err := json.Marshal(response)
    if err != nil {
        return false, nil, err
    }
    q := walletmodel.New(tx)
    row, err := q.CommitIdem(ctx, walletmodel.CommitIdemParams{
        Op:            op,
        Key:           key,
        RequestHash:   requestHash,
        ResponseJsonb: body,
    })
    if err != nil {
        return false, nil, err
    }
    return row.DidWrite, row.PriorResponse, nil
}
```

#### 7. 【源码验证 SOURCE-CHECK】
- Read `AILinkMax-Go/modules/wallet/business/idem.go:21-30`. Excerpt: `func (i *Idem) Reserve(ctx context.Context, op, key string) (bool, error) { ... return i.r.SetNX(ctx, fmt.Sprintf("wallet:idem:%s:%s", op, key), "1", idemTTL).Result() }`. **There is no Release/Commit; this is the entire surface.** Confirms M1.
- Read `migrations/` index to ensure `00035` is unused (run `ls migrations/00034*.sql` and `ls migrations/00035*.sql` — fail loud if 00035 exists, then bump per WS-14).

#### 8. 【如何验证 VERIFY】
1. `psql -f migrations/00035_idempotency_keys.sql $TEST_DSN` then `psql $TEST_DSN -c '\d+ idempotency_keys'` and check partitioning attributes present.
2. `cd modules/wallet/model && go generate ./...` (sqlc) must succeed without diff outside `sqlcgen/`.
3. `go test ./modules/wallet/model/ -run TestCommitIdem -race -count=20` — expected: all PASS; 1 000 parallel goroutines hammering the same `(op, key)` produce exactly one `didWrite=true` and 999 `didWrite=false` with identical `prior`.
4. `psql $TEST_DSN -c "SELECT count(*) FROM idempotency_keys WHERE op='deduct' AND key='unit-test-key';"` returns exactly `1`.

#### Mini-ADR
- Alternative considered: keep Redis SETNX, switch to Redis Lua to do "set, return prior" atomically.
- Why not: Redis is not in the wallet tx → Sentinel partition still produces a write-but-no-tx case; the whole bug class returns.
- Alternative considered: use `INSERT ... ON CONFLICT DO NOTHING RETURNING xmax` per v1 master plan.
- Why not: REV-FINAL-M2 — under READ COMMITTED the conflict row is invisible to the second writer's RETURNING; we lose `prior_response`. `DO UPDATE SET request_hash = idempotency_keys.request_hash` is the idiomatic Postgres workaround that turns conflict rows into update rows so RETURNING fires.

---

### TASK T02 — `Idem` interface re-shape (Commit-only)

#### 1. 【是什么 What it IS】
- A rewrite of `modules/wallet/business/idem.go`.
- New `Idem.Commit(ctx, tx, op, key, requestHash, response) (didWrite bool, prior []byte, err error)` that delegates to `model.CommitIdem`.
- Old `Reserve` method is **kept** alongside the new one for one release window (dual-write per T26 / C7). Both are called from service.go; service.go T03-T06 wires the new one inside tx and removes the old one after T26 cutover.

#### 2. 【不是什么 What it is NOT】
- NOT a removal of `Idem` struct or its constructor.
- NOT a change to the Redis namespace `wallet:idem:*`.
- NOT touching counter.go (which mediates Redis `counter:user:*`).

#### 3. 【为什么 Why this bug exists】
- M1 + REV-FINAL-M2: the contract had no "happened before; here is what to return" path. Adding Commit gives the wallet tx a single atomic side channel.

#### 4. 【做什么 DO】
1. Add `Commit` method to `Idem`.
2. Wire `*pgx.Tx`-aware delegate to `model.CommitIdem` (T01).
3. Add `IsNoopKey(key) bool` helper that returns true on empty key (callers may decide to fail the caller-facing op instead of silently succeeding — that decision lives in service.go, not here).

#### 5. 【不做什么 DON'T】
- Do NOT alter `Reserve`'s signature — its callers will be removed by T26, not refactored.
- Do NOT inject `*pgxpool.Pool` into Idem — Commit takes a tx supplied by the caller.

#### 6. 【怎么做 HOW (具体)】

`modules/wallet/business/idem.go` (append):

```go
import (
    walletmodel "github.com/cnxx-tech/ailinkmax-go/modules/wallet/model"
    "github.com/jackc/pgx/v5"
)

// Commit inserts the idempotency row inside the caller's tx. didWrite=true
// means the caller owns the row and should continue; didWrite=false means
// prior holds the historical response_jsonb. err is the PG error verbatim.
func (i *Idem) Commit(
    ctx context.Context,
    tx pgx.Tx,
    op, key, requestHash string,
    response any,
) (didWrite bool, prior []byte, err error) {
    if i == nil || key == "" {
        // Empty-key path is the caller's bug — return didWrite=true so the
        // caller proceeds; T07 enforces required-key for AdminAdjust at the
        // handler boundary, so this branch only fires from tests.
        return true, nil, nil
    }
    return walletmodel.CommitIdem(ctx, tx, op, key, requestHash, response)
}

// IsNoopKey lets callers (e.g. handlers) refuse to proceed on empty input.
func IsNoopKey(key string) bool { return key == "" }
```

#### 7. 【源码验证 SOURCE-CHECK】
- Read `modules/wallet/business/idem.go:1-30`. Confirm `Reserve` is the only method. Confirm the file does NOT currently import `pgx` — your addition will be the first.

#### 8. 【如何验证 VERIFY】
1. `go build ./modules/wallet/...` succeeds.
2. `go vet ./modules/wallet/...` zero warnings.
3. `go test ./modules/wallet/business/ -run TestIdem -race -count=10` passes.

#### Mini-ADR
- Alternative: replace `Reserve` entirely with `Commit`.
- Why not: T03-T06 callers also need to be migrated; doing both in one PR makes review impractical. Dual-write window (T26) is short (≤ 1 release).

---

### TASK T03 — Wallet `Deduct`: in-tx `idem.Commit`

#### 1. 【是什么 What it IS】
- An edit to `modules/wallet/business/service.go:Deduct` (line 230-340) that:
  - Removes the pre-tx `Reserve` short-circuit (lines 243-251).
  - Calls `s.idem.Commit(ctx, tx, "deduct", ch.IdemKey, requestHash, response)` inside `s.repo.InTx` after the balance update + tx-row + counter-outbox writes (i.e. just before the InTx callback returns nil).
  - Branches on `didWrite`: `true` → proceed; `false` → unmarshal `prior` into `DeductionResult` and return it as the InTx return value.

#### 2. 【不是什么 What it is NOT】
- NOT a re-implementation of double-bucket arithmetic. The gift/recharge math stays.
- NOT a change to the Redis `counter:user:*` mirror (counter.go).
- NOT an audit-event shape change.

#### 3. 【为什么 Why this bug exists】
- M1 root cause. Before the rewrite, idempotency lived outside the tx; tx rollback orphaned the Redis key.

#### 4. 【做什么 DO】
1. Compute `requestHash := canonicalSHA256(...)` over `{userID, amount, ch}` BEFORE entering InTx (cheap, deterministic).
2. Inside InTx, after balance update + insertCounterOutbox, build `response` (a `*DeductionResult`).
3. Call `idem.Commit`. On `didWrite=false`, decode prior; return decoded prior as the InTx result.
4. Adjust the outer-scope assignment so `result` ends up holding either the new computation or the prior.
5. Move the `counter.Incr` + `emitAudit` calls into a branch guarded by `didWrite=true` AND `!result.Insufficient` AND `actualDeduction > 0`.
6. Remove `Reserve` call.

#### 5. 【不做什么 DON'T】
- Do NOT keep the `Reserve` call alongside `Commit` — they would deadlock in the dual-write window because Reserve is fire-and-forget but Commit pre-checks. Choose one per op per release.
- Do NOT include cost basis or internal flags in `response`. Replay returns this verbatim to the client; if the client sees something they didn't see the first time, you've broken the contract.
- Do NOT log the prior payload at INFO level; it contains balance, which is PII-adjacent.

#### 6. 【怎么做 HOW (具体)】

Patch sketch (literal pseudo-diff against current line numbers in `service.go`):

```go
// Before (line 243-251):
if ok, err := s.idem.Reserve(ctx, "deduct", ch.IdemKey); err != nil {
    return contracts.DeductionResult{}, err
} else if !ok {
    w, e := s.EnsureWallet(ctx, userID)
    if e != nil { return contracts.DeductionResult{}, e }
    return contracts.DeductionResult{BalanceAfter: walletToBalance(w)}, nil
}

// After: delete this block. Compute hash once before InTx.
requestHash := canonicalHashDeduct(userID, amount, ch)
```

Inside the InTx callback, just before `return nil`:

```go
// Build final response. CRITICAL: contents replay back to clients on dup.
response := contracts.DeductionResult{
    BalanceAfter:           walletToBalance(after),
    GiftDeductedMicros:     giftDed,
    RechargeDeductedMicros: rechDed,
    Insufficient:           false,
}

didWrite, prior, ierr := s.idem.Commit(ctx, tx, "deduct", ch.IdemKey, requestHash, response)
if ierr != nil {
    return fmt.Errorf("wallet idem commit: %w", ierr)
}
if !didWrite {
    var p contracts.DeductionResult
    if err := json.Unmarshal(prior, &p); err != nil {
        return fmt.Errorf("wallet idem prior decode: %w", err)
    }
    result = p
    actualDeduction = 0  // do NOT incr counter, do NOT emit audit again
    audit = auditDetail{}
    return nil
}
result = response
return nil
```

Add helper `canonicalHashDeduct` in `service.go` or a new `service_hash.go`:

```go
func canonicalHashDeduct(userID string, amount int64, ch contracts.ChargeContext) string {
    // The hash must capture every input that meaningfully changes the side
    // effect. Order is fixed; new fields go at the end with versioning.
    h := sha256.New()
    fmt.Fprintf(h, "v1\x00%s\x00%d\x00%s\x00%s\x00%s\x00%s\x00",
        userID, amount, ch.ReasonCode, ch.LinkType, ch.LinkID, ch.IdemKey)
    return hex.EncodeToString(h.Sum(nil))
}
```

Note: `s.repo.InTx` currently passes `(ctx, *walletmodel.Queries)`. **Check the signature**: this plan assumes it can pass `pgx.Tx` too. If it can't, T01 already provides `walletmodel.New(tx)`; expose a helper `InTxRaw(ctx, func(tx pgx.Tx) error)` on `Repo` and use it here (Read `modules/wallet/model/repo.go` first to confirm).

#### 7. 【源码验证 SOURCE-CHECK】
- Read `modules/wallet/business/service.go:230-340`. Confirm the `Reserve`+`!ok` block is at lines 243-251 verbatim.
- Read `modules/wallet/model/repo.go` and find `InTx`. **Critical** — the existing signature must support tx access. If `InTx` only exposes `*walletmodel.Queries`, you must extend it to pass the raw `pgx.Tx` (or add a sibling `InTxRaw`). DO this in the same PR.

#### 8. 【如何验证 VERIFY】
1. `go test ./modules/wallet/business/ -run TestDeductIdempotency -race -count=10` — 1 000-way concurrency on same IdemKey ⇒ exactly one balance change.
2. Chaos: `tests/e2e/chaos/ws1/db_blip_test.go` (WS-13 harness) — DB SIGKILL mid-tx; expect: `wallets.recharge_balance_micros + gift_balance_micros` change equals `Σ cost over successful HTTP 200`, no row in `idempotency_keys` for failed tx, `wallet_transactions` rowcount equals number of successful responses.
3. `grep -n 'idem.Reserve' modules/wallet/business/service.go` returns the lines that remain inside the dual-write window for Credit/Refund/adminDebit/RecordUsage; T04-T06 will erase the rest.

#### Mini-ADR
- Alternative: keep `Reserve` and add `Commit` as best-effort sync after InTx.
- Why not: defeats the entire purpose; Reserve key still lives after rollback. M1 reappears.

---

### TASK T04 — Wallet `Credit`: in-tx `idem.Commit`

#### 1. 【是什么 What it IS】
- Mirror of T03 applied to `Credit` (line 387) and the package-private `credit` (line 399).

#### 2. 【不是什么 What it is NOT】
- NOT the place to fix Category default-resolution (line 444-456). That logic stays.

#### 3. 【为什么 Why this bug exists】
- Same M1 root.

#### 4. 【做什么 DO】
- Same pattern as T03, with `op = "credit"`. Response includes `BalanceAfter` and the assigned `BalanceType`.
- INVESTIGATE-004 (v2 §99): `Credit` op-namespace currently spans subscription bonus, refund-driven credit, and payment outbox. Before this task ships, run `grep -rn 'idem.Reserve.*"credit"' modules/` and `grep -rn 'IdemKey.*credit' modules/` and **list every distinct caller**. If the same `key` could collide across these callers (e.g. payment_id and subscription_id share an ID space), namespace by **adding a prefix to the key** at every callsite (`po_credit:<id>`, `sub_bonus:<id>`). Record the finding in the PR description; if collision is impossible, document why.

#### 5. 【不做什么 DON'T】
- Do NOT collapse op-namespaces by changing the `op` parameter — the response shape must match across callers; instead namespace the **key**.

#### 6. 【怎么做 HOW (具体)】
- Same pattern as T03; `op = "credit"`; `requestHash` covers `(userID, amount, cr, description)`.

#### 7. 【源码验证 SOURCE-CHECK】
- Read `service.go:387-482`. Confirm `s.idem.Reserve(ctx, "credit", cr.IdemKey)` at line 403.

#### 8. 【如何验证 VERIFY】
1. `go test ./modules/wallet/business/ -run TestCreditIdempotency -race -count=10`.
2. INVESTIGATE-004 written up in PR.

---

### TASK T05 — Wallet `Refund`: in-tx `idem.Commit`

#### 1. 【是什么 What it IS】
- Mirror of T03 for `Refund` (line 486).

#### 2. 【不是什么 What it is NOT】
- NOT the refund-double-approve fix (G2) — that lives in T09 around the refund_requests row, not the wallet write.

#### 3. 【为什么 Why this bug exists】
- M1.

#### 4. 【做什么 DO】
- Same pattern. `op = "refund"`; requestHash over `(userID, amount, rf)`.

#### 5. 【不做什么 DON'T】
- Do NOT alter the "refund over recharge balance" error at line 512-514.

#### 6. 【怎么做 HOW (具体)】
- Same as T03.

#### 7. 【源码验证 SOURCE-CHECK】
- `service.go:486-560`. Confirm `s.idem.Reserve(ctx, "refund", rf.IdemKey)` at line 498.

#### 8. 【如何验证 VERIFY】
1. `go test ./modules/wallet/business/ -run TestRefundIdempotency -race -count=10`.

---

### TASK T06 — Wallet `adminDebit` + `RecordUsage`: in-tx `idem.Commit`

#### 1. 【是什么 What it IS】
- Mirror of T03 for `adminDebit` (line 564) and `RecordUsage` (line 342).

#### 2. 【不是什么 What it is NOT】
- NOT a change to admin authz.

#### 4. 【做什么 DO】
- Same pattern; `op` is `"adjust_debit"` and `"record_usage"` respectively.

#### 5. 【不做什么 DON'T】
- Do NOT merge with T07 (AdminAdjust): that is a handler-side change, while this is service-side.

#### 6. 【怎么做 HOW (具体)】
- Same as T03.

#### 7. 【源码验证 SOURCE-CHECK】
- `service.go:564` (adminDebit), `service.go:342` (RecordUsage). Confirm Reserve calls at 568 and 354.

#### 8. 【如何验证 VERIFY】
1. `go test ./modules/wallet/business/ -run "TestAdminDebit|TestRecordUsage" -race -count=10`.

---

### TASK T07 — AdminAdjust: server-side namespaced idem key (REV-FINAL-M11)

#### 1. 【是什么 What it IS】
- Edit `modules/wallet/api/handlers_admin.go:adjust` (line 190-217) to **require** a client-supplied `X-Client-Nonce` header (BAD_REQUEST if missing).
- Compute server-side key = `sha256(adminID || walletID || balanceType || signedMicros || clientNonce)` (hex).
- Pass that key to `s.svc.AdminAdjust(...)`.
- Edit `service.go:AdminAdjust` (line 851) to accept that key directly (its current empty default is the bug).

#### 2. 【不是什么 What it is NOT】
- NOT trusting the client nonce as the key. The server namespaces by principal first.
- NOT enforcing UUID format on nonce — any non-empty string works; just enforce length 1-128.

#### 3. 【为什么 Why this bug exists】
- M4 + REV-FINAL-M11: trusting a raw client `Idempotency-Key` lets two admins (different actors) hit the same opaque string and collide; nonce-only also lets one admin's nonce replay against a different wallet.

#### 4. 【做什么 DO】
1. In the handler, read `r.Header.Get("X-Client-Nonce")`. If empty, `writeError(w, 400, "missing_client_nonce")` and `return`.
2. Build the key per HOW.
3. Pass the key to `AdminAdjust`.
4. Update `frontend/src/api/admin/wallet.ts` (per WS-11 — flag the change there). Frontend must generate a UUIDv4 per click and never reuse it across windows.

#### 5. 【不做什么 DON'T】
- Do NOT compute key in `service.go` — server-side key construction lives at the boundary so the audit log can attribute correctly.
- Do NOT accept `Idempotency-Key` as an alias (legacy header). That makes the change a no-op for clients still using it.

#### 6. 【怎么做 HOW (具体)】

In `handlers_admin.go:adjust`:

```go
nonce := r.Header.Get("X-Client-Nonce")
if nonce == "" || len(nonce) > 128 {
    writeError(w, http.StatusBadRequest, "missing_or_invalid_client_nonce")
    return
}
h := sha256.New()
fmt.Fprintf(h, "admin_adjust\x00%s\x00%s\x00%s\x00%d\x00%s\x00",
    c.UserID, id, in.BalanceType, micros, nonce)
idemKey := hex.EncodeToString(h.Sum(nil))

updated, lastTx, err := h.svc.AdminAdjust(r.Context(), id, c.UserID, in.BalanceType, in.Description, micros, idemKey)
```

In `service.go:AdminAdjust`, remove the special-casing of empty `idemKey` (callers must pass non-empty after T07; tests must update).

#### 7. 【源码验证 SOURCE-CHECK】
- Read `handlers_admin.go:202`. Confirm the call site is `h.svc.AdminAdjust(r.Context(), id, c.UserID, in.BalanceType, in.Description, micros, "")` — final argument is the empty string. M4 confirmed.

#### 8. 【如何验证 VERIFY】
1. Curl: `curl -X POST localhost:8080/api/admin/wallets/wal_X/adjust ...` with **no** `X-Client-Nonce` → expect 400 `missing_or_invalid_client_nonce`.
2. Two identical POSTs with same nonce → balance changes once.
3. Two identical POSTs with different nonces by different admins → balance changes twice (intentional — distinct actors).

---

### TASK T08 — payment_outbox + processor + webhook rewrite (M2 / G4 / REV-FINAL-M1 / REV-FINAL-M5)

#### 1. 【是什么 What it IS】
- Migration `migrations/00036_payment_outbox.sql`.
- New service `modules/payment/business/outbox.go` (`PaymentOutboxProcessor`).
- Rewritten `webhook.go:Receive` to NOT call `orders.Credit` synchronously; instead, write `payment_orders.status='succeeded'` AND insert into `payment_outbox(payment_id, payload, status='pending')` in **one** tx.
- New worker entry `binaries/worker-billing/main.go` (add) or `binaries/all-in-one/main.go` (add) — spawn `PaymentOutboxProcessor.Run` via `runtime.Supervised`.

#### 2. 【不是什么 What it is NOT】
- NOT a change to webhook signature verification — that's WS-5 / T_BACKFILL_PAYMENT scope.
- NOT introducing CDC. Outbox is poll-based (30 s tick), per master plan.

#### 3. 【为什么 Why this bug exists】
- M2: webhook returns 200 even on wallet failure; platform never retries.
- G4: same root.
- REV-FINAL-M1: processor needs `FOR UPDATE SKIP LOCKED` because multiple binaries (worker-billing AND all-in-one) might both claim rows.
- REV-FINAL-M5: outbox processor must check the idempotency_keys row's `request_hash` matches `sha256(canonical(payload))`; otherwise it's "succeeded but admin edited the payload" and merits an alert, not mark-committed.

#### 4. 【做什么 DO】
1. Migration: create `payment_outbox` table.
2. Down: drop table (idempotent processor will rebuild from `payment_orders` if needed).
3. Sqlc query file `modules/payment/model/queries/payment_outbox.sql`:
   - `InsertPaymentOutbox`
   - `ClaimPendingPaymentOutbox(limit int) [{id, payment_id, payload, attempt}]` — `SELECT ... FOR UPDATE SKIP LOCKED LIMIT $1`
   - `MarkPaymentOutboxCommitted(id)`
   - `BumpPaymentOutboxAttempt(id, nextAt int64)`
4. Edit `webhook.go:Receive`:
   - On `deposit + completed/succeeded` branch: **inside a new InTx**, (a) call `orders.transitionOrder(... to='succeeded' ...)` and (b) insert the outbox row carrying the credit payload.
   - Remove the synchronous `orders.Credit` call.
   - Return HTTP 5xx (caller will write 5xx) when the InTx itself fails (lets the gateway retry).
   - On duplicate webhook (cb conflict) **read** `payment_outbox.status`: `committed` → 200; `pending` AND `created_at > 5min ago` → 5xx; else → 200. This is REV-FINAL-M1 behavior.
5. New `outbox.go`:
   - `Run(ctx)` ticks every 30 s.
   - Each tick: `ClaimPendingPaymentOutbox(100)`; for each:
     - Compute `expectedHash := canonicalHashCredit(payload)`.
     - In a **separate** wallet-side tx: call `s.wallet.Credit(...)` — this internally calls `idem.Commit` (T04) with `op="credit"`, `key="po_credit:<payment_id>"`, `requestHash=expectedHash`.
     - If wallet returned didWrite=false AND the prior's request_hash differs → alert + DLQ (REV-FINAL-M5).
     - Mark committed.
     - On error: bump attempt; `next_attempt_at = now + 2^min(attempt, 10) seconds + jitter`.

#### 5. 【不做什么 DON'T】
- Do NOT use plain `FOR UPDATE` — single slow worker would block siblings. SKIP LOCKED is essential.
- Do NOT call `idem.Commit` in `outbox.go` directly — go through `wallet.Credit` so the tx-row + counter-outbox writes happen in the same wallet tx as the idem write. This preserves Single-Writer invariant.
- Do NOT delete committed rows on success — keep them for audit and replay; weekly partition prune by `created_at`.
- Do NOT alarm on first-attempt fail — only after `attempt > 5` to avoid noise during transient outages.

#### 6. 【怎么做 HOW (具体)】

`migrations/00036_payment_outbox.sql`:

```sql
-- +goose Up
-- +goose StatementBegin
CREATE TABLE payment_outbox (
    id              bigserial PRIMARY KEY,
    payment_id      text         NOT NULL UNIQUE,
    payload         jsonb        NOT NULL,
    request_hash    text         NOT NULL,
    status          text         NOT NULL
                    CHECK (status IN ('pending','committed','failed','dlq')),
    attempt         int          NOT NULL DEFAULT 0,
    next_attempt_at timestamptz  NOT NULL DEFAULT now(),
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX payment_outbox_due_idx
  ON payment_outbox (next_attempt_at)
  WHERE status = 'pending';
-- +goose StatementEnd
-- +goose Down
DROP TABLE IF EXISTS payment_outbox;
```

Sqlc query (truncated for brevity — see file for full):

```sql
-- name: ClaimPendingPaymentOutbox :many
SELECT id, payment_id, payload, request_hash, attempt
FROM payment_outbox
WHERE status = 'pending' AND next_attempt_at <= now()
ORDER BY next_attempt_at
LIMIT $1
FOR UPDATE SKIP LOCKED;
```

In `webhook.go:Receive`, replace the `case "completed", "succeeded":` body with a call to a new helper:

```go
func (s *WebhookService) handleSucceededDeposit(ctx context.Context, n NormalizedWebhook) error {
    payload := map[string]any{
        "payment_id":      n.PaymentID,
        "wallet_id":       n.WalletID,
        "amount_micros":   n.AmountMicros,
        "gateway_trade":   n.ProviderTradeID,
        "raw":             n.RawPayload,
    }
    body, _ := json.Marshal(payload)
    requestHash := canonicalHashCredit(payload)
    return s.repo.InTx(ctx, func(ctx context.Context, q *paymentmodel.Queries, _ pgx.Tx) error {
        // transition payment_orders.status = 'succeeded'
        _, err := q.UpdatePaymentOrderStatus(ctx, paymentmodel.UpdatePaymentOrderStatusParams{
            Status:    "succeeded",
            PaidAt:    time.Now().UnixMilli(),
            CreditedAt: 0,  // outbox flips this when wallet ack
            ID:        n.PaymentID,
            // ... gateway fields ...
        })
        if err != nil { return err }
        _, err = q.InsertPaymentOutbox(ctx, paymentmodel.InsertPaymentOutboxParams{
            PaymentID:    n.PaymentID,
            Payload:      body,
            RequestHash:  requestHash,
            Status:       "pending",
        })
        return err
    })
}
```

The duplicate-webhook branch (cb conflict) reads `payment_outbox.status` and decides the HTTP code. Handler-side translation:

```go
case errors.Is(err, ErrDuplicateWebhook):
    st, _ := s.repo.Queries().GetPaymentOutboxStatus(ctx, n.PaymentID)
    switch st {
    case "committed":  return cb, nil       // 200
    case "pending":
        if time.Since(outbox.CreatedAt) < 5*time.Minute {
            return cb, nil                   // 200 — too soon to retry
        }
        return cb, ErrWebhookRetryPlease     // → 5xx by handler
    default:
        return cb, nil
    }
```

`outbox.go` `Run`:

```go
func (p *PaymentOutboxProcessor) Run(ctx context.Context) error {
    t := time.NewTicker(30 * time.Second)
    defer t.Stop()
    for {
        select {
        case <-ctx.Done(): return ctx.Err()
        case <-t.C: p.drain(ctx)
        }
    }
}

func (p *PaymentOutboxProcessor) drain(ctx context.Context) {
    rows, err := p.repo.ClaimPendingPaymentOutbox(ctx, 100)  // FOR UPDATE SKIP LOCKED
    if err != nil { p.log.Error(...); return }
    for _, row := range rows {
        payload, _ := decode(row.Payload)
        idemKey := "po_credit:" + row.PaymentID
        if err := p.wallet.Credit(ctx, payload.UserID, payload.AmountMicros, contracts.CreditContext{
            Category: contracts.CategoryRecharge,
            ReasonCode: contracts.ReasonTopupGateway,
            BalanceType: contracts.BalanceTypeRecharge,
            LinkType: contracts.LinkTypePaymentOrder,
            LinkID: row.PaymentID,
            IdemKey: idemKey,
        }); err != nil {
            backoff := time.Duration(math.Pow(2, math.Min(float64(row.Attempt), 10))) * time.Second
            _ = p.repo.BumpPaymentOutboxAttempt(ctx, row.ID, time.Now().Add(backoff).UnixMilli())
            if row.Attempt > 5 { p.log.Error("payment outbox stuck", ...); /* alert */ }
            continue
        }
        // REV-FINAL-M5: prior hash check — if Credit succeeded but the idem row says
        // a *different* request_hash, the payload was edited between webhook and processor.
        // (Implementation detail: extend wallet.Credit's response to surface priorHash.)
        _ = p.repo.MarkPaymentOutboxCommitted(ctx, row.ID)
    }
}
```

#### 7. 【源码验证 SOURCE-CHECK】
- Read `webhook.go:97-115` to confirm the synchronous `s.orders.Credit(...)` and `markProcessed(cb.ID, cerr)` path (line 100 and 105). M2 confirmed.
- Read `orders.go:160-202`. Confirm tx commit at line 161, wallet call at line 174 outside tx.

#### 8. 【如何验证 VERIFY】
1. `migrate up; psql -c '\d payment_outbox'` shows the table.
2. Unit `TestPaymentOutboxIdempotent` posts the same webhook 1 000×; wallet balance changes exactly once; `payment_outbox` has one row in state `committed`.
3. Chaos: Redis kill during processor tick — outbox row stays `pending`, `attempt` increments, retry succeeds; `idempotency_keys('credit', 'po_credit:<id>')` has exactly one row.
4. Duplicate webhook stalled in `pending > 5min` returns 5xx (curl test).

---

### TASK T09 — refund_outbox + Refund.Create double-check (G2 / M3 / REV-FINAL-M4)

#### 1. 【是什么 What it IS】
- Migration `migrations/00037_refund_outbox.sql` mirroring T08.
- New `RefundOutboxProcessor` mirroring T08's PaymentOutboxProcessor.
- Edit `refunds.go:Create` (line 54) to do (a) PG advisory lock `pg_advisory_xact_lock(hashtext('refund:' || wallet_id))` (b) `SELECT COALESCE(SUM(amount_micros),0) FROM refund_requests WHERE wallet_id=$1 AND status IN ('pending_approval','approved','processing','succeeded')` (c) require `bal.RechargeMicros - sum >= AmountMicros` else return `ErrRefundExceedsBalance`.
- Edit `refunds.go:Complete` (line 130) to NOT call `s.wallet.Refund` synchronously; instead write `refund_outbox(refund_id, payload, status='pending')` in the same tx as the `processing → succeeded` transition.
- Add front-gate in `refunds.go:Create`: query `payment_outbox.status` of the originating payment; reject with `ErrPaymentStillProcessing` unless `committed` (or `payment_order_id == ''`, which is wallet-only refund).

#### 2. 【不是什么 What it is NOT】
- NOT serializing all refund requests. The advisory lock is per-wallet only.

#### 3. 【为什么 Why this bug exists】
- G2: no SUM check across pending refund requests; admin can double-approve.
- M3: status reaches `succeeded` before wallet write.
- REV-FINAL-M4: a plain wallet-row lock doesn't help here because the pending-refund check spans multiple refund_request rows; an advisory lock is the cheapest correct option.

#### 4. 【做什么 DO】
1. Migration mirroring T08.
2. Sqlc: `ClaimPendingRefundOutbox` with `FOR UPDATE SKIP LOCKED`.
3. Update `Create` to use advisory lock + SUM check + payment_outbox check.
4. Update `Complete` to write outbox row instead of calling `s.wallet.Refund`.
5. Implement `RefundOutboxProcessor` calling `wallet.Refund` with `idem.Commit` (via T05).

#### 5. 【不做什么 DON'T】
- Do NOT use SERIALIZABLE isolation — too expensive and rare bug compared to advisory lock.
- Do NOT block Create on Complete's outbox — they are independent flows.

#### 6. 【怎么做 HOW (具体)】

Inside `Create`'s repo.InTx (wrap existing body):

```go
err := s.repo.InTx(ctx, func(ctx context.Context, q *paymentmodel.Queries, tx pgx.Tx) error {
    // 1) per-wallet advisory lock
    if _, err := tx.Exec(ctx, "SELECT pg_advisory_xact_lock(hashtextextended('refund:'||$1, 0))", walletID); err != nil {
        return err
    }
    // 2) sum-of-open refunds
    var openSum int64
    if err := tx.QueryRow(ctx, `
        SELECT COALESCE(SUM(amount_micros),0) FROM refund_requests
        WHERE wallet_id=$1 AND status IN ('pending_approval','approved','processing','succeeded')`,
        walletID).Scan(&openSum); err != nil { return err }
    if bal.RechargeMicros < in.AmountMicros + openSum {
        return ErrRefundExceedsBalance
    }
    // 3) payment_outbox front-gate
    if in.PaymentOrderID != "" {
        var st string
        if err := tx.QueryRow(ctx, `SELECT status FROM payment_outbox WHERE payment_id=$1`, in.PaymentOrderID).Scan(&st); err == nil {
            if st != "committed" { return ErrPaymentStillProcessing }
        } else if !errors.Is(err, pgx.ErrNoRows) {
            return err
        }
    }
    // 4) original CreateRefundRequest call
    row, err = q.CreateRefundRequest(ctx, ... )
    return err
})
```

Migration `00037_refund_outbox.sql`: same shape as T08, with `refund_id` instead of `payment_id`.

#### 7. 【源码验证 SOURCE-CHECK】
- Read `refunds.go:54-109`. Confirm balance check uses `bal.RechargeMicros < in.AmountMicros` ONLY at line 62 — no `openSum` consideration.
- Read `refunds.go:130-167`. Confirm state→succeeded then wallet.Refund order. Comment at 161-167 acknowledges under-refund risk.

#### 8. 【如何验证 VERIFY】
1. `tests/e2e/refund/concurrent_admin_approve_test.go`: two admins simultaneously approve two refunds totaling > recharge; expect exactly one to succeed, the other to return ErrRefundExceedsBalance.
2. Wallet refund only happens after outbox processor commits.

---

### TASK T10 — Subscription quota CheckAndApply (G1)

#### 1. 【是什么 What it IS】
- New method `SubscriptionService.CheckAndApply(ctx, userID, costMicros) (QuotaResult, error)` that, in a single tx with `SELECT ... FROM user_subscriptions WHERE user_id=$1 AND status='active' ORDER BY created_at DESC LIMIT 1 FOR UPDATE`, decides quota source AND, if `Subscription` covered, immediately runs `UPDATE user_subscriptions SET cycle_used_micros = cycle_used_micros + $2 WHERE id=$1`.
- Migration `migrations/00040_user_subscriptions_quota_check.sql` adding `CHECK (cycle_used_micros <= cycle_quota_micros + COALESCE(overage_cap_micros, 0))`.

#### 2. 【不是什么 What it is NOT】
- NOT a billing-side change. Billing's `quota.go` Check stays; it becomes a read-only preview. The mutating path moves to subscription module.

#### 3. 【为什么 Why this bug exists】
- G1: `quota.go:Check` (lines 68-90) is read-only; the apply is elsewhere; without `FOR UPDATE` and without single-tx coupling, two concurrent requests at quota boundary both see "covered" and both apply, overshooting quota.

#### 4. 【做什么 DO】
1. Add method to `SubscriptionService` returning the new shape.
2. Adjust billing callers (search `quota.Check` callers and replace with `subs.CheckAndApply` where the caller actually applies).
3. Add CHECK constraint via migration; ensure tests don't write violating rows.

#### 5. 【不做什么 DON'T】
- Do NOT remove `decideQuotaSource` — it's pure and unit-testable, reuse it inside the new method.
- Do NOT add the CHECK without a `overage_cap_micros` column — confirm one exists first (`grep -n overage_cap modules/subscription/model/`). If missing, add a nullable column.

#### 6. 【怎么做 HOW (具体)】

```go
func (s *SubscriptionService) CheckAndApply(ctx context.Context, userID string, cost int64) (QuotaResult, error) {
    tx, err := s.pool.Begin(ctx); if err != nil { return QuotaResult{}, err }
    defer tx.Rollback(ctx) //nolint:errcheck
    var subID, policy string
    var quota, used int64
    err = tx.QueryRow(ctx, `
        SELECT us.id, us.cycle_quota_micros, us.cycle_used_micros, sp.overage_policy
        FROM user_subscriptions us JOIN subscription_products sp ON sp.id = us.product_id
        WHERE us.user_id=$1 AND us.status='active'
        ORDER BY us.created_at DESC LIMIT 1
        FOR UPDATE`, userID).Scan(&subID, &quota, &used, &policy)
    if errors.Is(err, pgx.ErrNoRows) {
        return QuotaResult{Source: QuotaSourceNone}, tx.Commit(ctx)
    } else if err != nil { return QuotaResult{}, err }
    res := decideQuotaSource(true, subID, policy, quota, used, cost)
    if res.Source == QuotaSourceSubscription {
        if _, err := tx.Exec(ctx, `UPDATE user_subscriptions SET cycle_used_micros = cycle_used_micros + $2 WHERE id=$1`, subID, cost); err != nil {
            return QuotaResult{}, err
        }
    }
    return res, tx.Commit(ctx)
}
```

#### 7. 【源码验证 SOURCE-CHECK】
- `quota.go:73-78`. Confirm no `FOR UPDATE`.
- `subscription.go:1-50`. Confirm `*pgxpool.Pool` is accessible to `SubscriptionService`.

#### 8. 【如何验证 VERIFY】
1. 1 000 parallel requests against a sub with quota = 1 000 000; expect `cycle_used_micros` ≤ `cycle_quota_micros`; PG CHECK guards against any code path that bypasses.

---

### TASK T11 — Subscription Cancel status guard (G5)

#### 1. 【是什么 What it IS】
- Edit `subscription.go:Cancel` (line 100) UPDATE statements to add `AND status='active'`; check `RowsAffected()`; return `ErrSubscriptionNotActive` if 0.

#### 4. 【做什么 DO】
- Apply to both immediate-cancel and cancel-at-period-end UPDATEs (lines 124, 132).

#### 6. 【怎么做 HOW (具体)】

```go
tag, err := s.pool.Exec(ctx,
    `UPDATE user_subscriptions
     SET status='canceled', end_reason='canceled_immediate', canceled_at=$2, updated_at=$2
     WHERE id=$1 AND status='active'`, subID, now)
if err != nil { return nil, err }
if tag.RowsAffected() == 0 { return nil, ErrSubscriptionNotActive }
```

#### 7. 【源码验证 SOURCE-CHECK】
- `subscription.go:124-127, 132-135`. Confirm WHERE has no status predicate.

#### 8. 【如何验证 VERIFY】
1. Concurrent Cancel + Upgrade: at most one succeeds; the loser returns its respective ErrInvalidState/ErrSubscriptionNotActive.

---

### TASK T12 — RollIfDue re-read inside tx (G6)

#### 1. 【是什么 What it IS】
- Edit `cycle.go:RollIfDue` (line 34) to, **after `Begin`**, re-`SELECT cancel_at_period_end, ends_at, current_cycle_end FROM user_subscriptions WHERE id=$1 FOR UPDATE` and use those values in the branch.

#### 4. 【做什么 DO】
- Take a per-row lock (`FOR UPDATE`) at the top of the tx; if `now <= current_cycle_end` (just re-checked), `Commit` and return `false`.

#### 6. 【怎么做 HOW (具体)】

```go
tx, err := c.pool.Begin(ctx); if err != nil { return false, err }
defer tx.Rollback(ctx) //nolint:errcheck

var capEnd int64; var cape bool; var ends int64; var used int64; var quota int64
err = tx.QueryRow(ctx,
    `SELECT current_cycle_end, cancel_at_period_end, ends_at, cycle_used_micros, cycle_quota_micros
     FROM user_subscriptions WHERE id=$1 AND status='active' FOR UPDATE`, subID).
    Scan(&capEnd, &cape, &ends, &used, &quota)
if errors.Is(err, pgx.ErrNoRows) { return false, tx.Commit(ctx) }
if err != nil { return false, err }
if now <= capEnd { return false, tx.Commit(ctx) }
// ... rest of branches using the freshly-read values ...
```

#### 7. 【源码验证 SOURCE-CHECK】
- `cycle.go:34-114`. Confirm `sub.CancelAtPeriodEnd` and `sub.EndsAt` are referenced from the pre-`Begin` copy.

#### 8. 【如何验证 VERIFY】
1. Test: concurrent `Cancel(Immediate=true)` + scheduler `RollIfDue`; expect single state transition; `subscription_cycles` rowcount matches.

---

### TASK T13 — subscription_upgrade_outbox (G7 / REV-FINAL-M10)

#### 1. 【是什么 What it IS】
- Migration `migrations/00038_subscription_upgrade_outbox.sql`.
- `SubscriptionUpgradeOutboxProcessor` worker.
- Edit `subscription.go:Upgrade` (line 145) to:
  - In a tx, UPDATE old sub status to `'upgrading'` (NEW status — add to status enum / CHECK constraint).
  - Insert outbox row carrying `PurchaseInput`.
  - Commit. Return a ticket-like response (`UpgradeOutput` synthesized with `Subscription` showing the old sub in `upgrading` state and a `pending=true` flag).
- Processor: in its own loop, claims pending outbox rows with SKIP LOCKED; calls `s.orders.Purchase`; on success updates outbox + old sub status to `expired`; on permanent failure (after attempt > 5) reverts old sub back to `active` and alerts.

#### 2. 【不是什么 What it is NOT】
- NOT a cross-module sync call from within a tx — that would violate the no-cross-import rule.
- NOT a removal of `Upgrade`'s API surface — the public contract becomes "Upgrade enqueued; poll for completion".

#### 3. 【为什么 Why this bug exists】
- G7: today `Upgrade` does an UPDATE-to-expired then calls `Purchase` separately; if `Purchase` fails, the old sub is already gone.
- REV-FINAL-M10: the v1 "single tx across modules" plan violated `scripts/check-mod-deps.sh`. Outbox is the only path.

#### 4. 【做什么 DO】
1. Migration: outbox table + add `'upgrading'` to status CHECK if currently constrained.
2. `Upgrade` only writes outbox; returns a synthesized response.
3. Processor calls Purchase.

#### 5. 【不做什么 DON'T】
- Do NOT block client until Purchase completes — Purchase may include payment-gateway calls; webhook-like async is correct.
- Do NOT keep the old code's `s.orders.Purchase` call in `Upgrade`.

#### 6. 【怎么做 HOW (具体)】

Migration shape (same as T08 + status update):

```sql
CREATE TABLE subscription_upgrade_outbox (
    id          bigserial PRIMARY KEY,
    old_sub_id  text         NOT NULL UNIQUE,
    payload     jsonb        NOT NULL,
    status      text         NOT NULL CHECK (status IN ('pending','committed','failed','dlq')),
    attempt     int          NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    created_at  timestamptz  NOT NULL DEFAULT now(),
    updated_at  timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX subscription_upgrade_outbox_due_idx ON subscription_upgrade_outbox (next_attempt_at) WHERE status='pending';

-- If status is checked: extend CHECK.
ALTER TABLE user_subscriptions DROP CONSTRAINT IF EXISTS user_subscriptions_status_check;
ALTER TABLE user_subscriptions ADD CONSTRAINT user_subscriptions_status_check
    CHECK (status IN ('pending_payment','active','canceled','expired','upgrading'));
```

#### 7. 【源码验证 SOURCE-CHECK】
- `subscription.go:145-181`. Confirm UPDATE-to-expired at lines 163-167 then call to `s.orders.Purchase` at line 169 across separate calls (no shared tx).
- INVESTIGATE-006 (v2 §99) flagged: does `shared/contracts` expose a tx-receiving Purchase? **If yes**, an alternative is to keep `Upgrade` in-process and pass the tx. Inspect `shared/contracts/subscription.go` first. If the tx-aware variant exists, weigh it against the outbox in the mini-ADR; default to outbox (cross-module simplicity > cross-module tx coupling).

#### 8. 【如何验证 VERIFY】
1. Chaos: kill the binary between `Upgrade` write and processor pick-up; old sub state observable as `upgrading` (NOT `active`, NOT `expired`); after restart, processor completes Purchase or, after 5 fails, reverts to `active` with alert.

---

### TASK T14 — mgmttoken Blacklist.Revoke returns error (G10)

#### 1. 【是什么 What it IS】
- Edit `modules/mgmttoken/model/blacklist.go:Revoke` (line 33) to return `error`. Update the interface (`BlacklistWriter` in `business/service.go:106`) and all call sites.

#### 4. 【做什么 DO】
1. Change signature: `Revoke(ctx, tokenID, ttl) error`.
2. Replace `_ = b.r.Set(...).Err()` with `return b.r.Set(...).Err()`.
3. Call sites (`grep -n Revoke modules/mgmttoken/business/`) propagate the error; on Redis err, return HTTP 503 to caller (fail-closed).

#### 5. 【不做什么 DON'T】
- Do NOT silently swallow the error in a defer.
- Do NOT add a local in-memory blacklist as a fallback — that creates a per-instance inconsistency Rust gateway can't see.

#### 6. 【怎么做 HOW (具体)】

```go
func (b *Blacklist) Revoke(ctx context.Context, tokenID string, ttl time.Duration) error {
    if b == nil || b.r == nil || tokenID == "" { return nil }
    if ttl <= 0 { return nil }
    return b.r.Set(ctx, MgmtBlacklistPrefix+tokenID, "1", ttl).Err()
}
```

Update `BlacklistWriter` interface in `business/service.go`:
```go
type BlacklistWriter interface {
    Revoke(ctx context.Context, tokenID string, ttl time.Duration) error
}
```
All callers must check the error.

#### 7. 【源码验证 SOURCE-CHECK】
- `blacklist.go:33-41`. Confirm `_ = b.r.Set(...).Err()`. G10 confirmed.
- `business/service.go:105-107`. Confirm `BlacklistWriter` interface no-return shape.

#### 8. 【如何验证 VERIFY】
1. Unit: inject failing redis; Revoke returns error; service returns 503; mgmt token still active in cache for 5 min is the failure mode we're now avoiding because the caller now retries or alerts.

---

### TASK T15 — wallet_counter_outbox commit timestamp + relay ordering (G3 / REV-004a)

#### 1. 【是什么 What it IS】
- Migration `migrations/00039_wallet_outbox_commit_ts.sql` adding `commit_timestamp timestamptz NOT NULL DEFAULT clock_timestamp()` and an index `(commit_timestamp, id)`.
- Edit `wallet/model/queries/wallet_counter_outbox.sql:ListUnprocessedWalletCounterOutbox` to `ORDER BY commit_timestamp ASC, id ASC`.
- Edit `counter_relay.go:135` to call `cr.rdb.Set(ctx, key, row.BalanceMicro, 24*time.Hour)` (TTL bound; cluster prune).

#### 4. 【做什么 DO】
1. Migration adds the column on insert (default `clock_timestamp()`); backfill historical rows with `created_at::timestamptz`.
2. Sqlc query rewrite.
3. Relay TTL switch.

#### 5. 【不做什么 DON'T】
- Do NOT use `pg_xact_commit_timestamp()` unless `track_commit_timestamp = on` is verified in production. If it's off, that path errors; fall back to `clock_timestamp()` at INSERT time (slightly less accurate but always present). Verify with `SHOW track_commit_timestamp;` on a staging DB BEFORE shipping.

#### 6. 【怎么做 HOW (具体)】

```sql
-- +goose Up
ALTER TABLE wallet_counter_outbox ADD COLUMN commit_timestamp timestamptz NOT NULL DEFAULT clock_timestamp();
UPDATE wallet_counter_outbox SET commit_timestamp = to_timestamp(created_at / 1000.0) WHERE commit_timestamp IS NULL;
CREATE INDEX wallet_counter_outbox_commit_ts_idx ON wallet_counter_outbox (commit_timestamp, id) WHERE processed_at = 0;
```

In sqlc query for `ListUnprocessedWalletCounterOutbox`:
```sql
SELECT id, user_id, balance_micro
FROM wallet_counter_outbox
WHERE processed_at = 0
ORDER BY commit_timestamp ASC, id ASC
LIMIT $1;
```

`counter_relay.go:135`:
```go
if err := cr.rdb.Set(ctx, key, row.BalanceMicro, 24*time.Hour).Err(); err != nil { ... }
```

#### 7. 【源码验证 SOURCE-CHECK】
- `counter_relay.go:135`. Confirm `cr.rdb.Set(ctx, key, row.BalanceMicro, 0)` (TTL 0 = no expiry).
- `modules/wallet/model/queries/wallet_counter_outbox.sql` (read full file) — confirm current ORDER BY.

#### 8. 【如何验证 VERIFY】
1. Insert 100 outbox rows with skewed `created_at` (simulate clock jump); verify processing order matches `commit_timestamp` strict-ascending.
2. Redis: 24 h after a write, key gone; before, key present.

---

### TASK T16 — Rust LeaseGuard refund-on-Drop + logging.rs Err paths (Billing-1/5/6 / REV-FINAL-M3 / C3)

#### 1. 【是什么 What it IS】
- Edit `crates/gw-ledger/src/lease.rs`:
  - Add `settled: AtomicBool` to `LeaseGuard` (placement: between `released` and `state`).
  - Make `settle_actual` idempotent: `if self.settled.swap(true, AcqRel) { return; }` at top, then existing logic.
  - In `release`, after the CAS sets `released=true`, check `if !self.settled.load(Acquire) { self.settle_actual(0); }`. This refunds the full reservation when no settle has happened.
  - `Drop` unchanged (still calls `release`).
- Edit `crates/gw-proxy/src/filters/logging.rs`:
  - Line 169-176 Err branch: before `g.release();`, emit `UsageRecord { vendor_exposure_state: Terminated, credits_consumed: Some(0), ... }` (C3 plumbed).
  - Line 114 / 146 CLI early-return branches: before `g.release();`, emit same Terminated UsageRecord.
- Update `gw_ledger::usage::UsageRecord` doc string for `Terminated` to formalize `credits_consumed: Some(0)` invariant. (Spec also referenced by T25.)

#### 2. 【不是什么 What it is NOT】
- NOT a change to `RequestFreeze` — that one already has the refund-on-Drop pattern (this is the pattern to mirror).
- NOT a change to the protected_mode HTTP code path — WS-6 owns response side.

#### 3. 【为什么 Why this bug exists】
- Billing-1, -5, -6 share the root cause: `settle_actual` is not idempotent AND `release` does not refund. Any `?`-early-return path leaks credits.

#### 4. 【做什么 DO】
1. Read `lease.rs:117-216` carefully. Note: in this codebase, `LeaseGuard` is **not** RAII-refunding by default (unlike `RequestFreeze` at `freeze.rs:128-141`).
2. Before adding fields, run `rg 'settle_actual\(' crates/` and list every call site. If there is a "settle estimate then settle final" pattern, the simple "idempotent-via-swap" makes the second call a no-op, which is wrong.
3. Implement the candidate per HOW.
4. Update `lease.rs:settle_actual_le_reserved_refunds_difference` test (line 354-363) to cover Drop-then-second-settle = no-op.
5. Add new tests: drop-without-settle refunds full reservation; settle-then-drop is no-op.

#### 5. 【不做什么 DON'T】
- Do NOT replace `release` semantics — it must stay idempotent for existing callers.
- Do NOT call `enqueue_frozen_delta` from `LeaseGuard` — that's `RequestFreeze`'s domain.

#### 6. 【怎么做 HOW (具体)】

```rust
pub struct LeaseGuard {
    uid: UserId,
    reserved: i64,
    coef_version: u64,
    released: AtomicBool,
    settled: AtomicBool,        // NEW
    state: Arc<LeaseState>,
}

// inside try_acquire, replace the struct literal:
return Ok(LeaseGuard {
    uid: uid.clone(),
    reserved: requested,
    coef_version,
    released: AtomicBool::new(false),
    settled: AtomicBool::new(false),  // NEW
    state,
});

impl LeaseGuard {
    pub fn settle_actual(&self, actual: i64) {
        if self.settled.swap(true, Ordering::AcqRel) { return; }   // NEW idempotency
        let actual = actual.max(0);
        if actual >= self.reserved { return; }
        let refund = self.reserved - actual;
        self.state.credits.fetch_add(refund, Ordering::AcqRel);
    }

    pub fn release(&self) {
        if self.released.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        // NEW: if Drop happens before any settle, refund full reservation.
        if !self.settled.load(Ordering::Acquire) {
            self.settle_actual(0);
        }
    }
}
```

In `logging.rs:163-180` Err branch, replace:
```rust
warn!(target: "gw_proxy", "credit-lease actual_usage_credits failed; settle skipped");
g.release();
```
with:
```rust
// C3: emit Terminated UsageRecord with credits_consumed = Some(0).
emit_usage_record_terminated(ctx, /* credits_consumed */ 0);
g.release();  // refunds full reservation via Drop-Path coupling in lease.rs
```
Implement `emit_usage_record_terminated` near-by (likely in `logging.rs`):
```rust
fn emit_usage_record_terminated(ctx: &ReqCtx, credits_consumed: i64) {
    let rec = build_usage_record(ctx, |r| {
        r.vendor_exposure_state = VendorExposureState::Terminated;
        r.credits_consumed = Some(credits_consumed);
    });
    let _ = ctx.ledger.emit(LedgerItem { record: rec, clear: None });
}
```
(`build_usage_record` is presumed to exist or be extracted from the success path; if it doesn't, factor out the field-population into a helper.)

#### 7. 【源码验证 SOURCE-CHECK】
- `lease.rs:117-123`. Confirm fields `{uid, reserved, coef_version, released, state}`. **No `settled`** — confirmed.
- `lease.rs:199-206` (`settle_actual`). Confirm no idempotency guard.
- `lease.rs:211-215` (`release`). Confirm pure CAS marker, no refund.
- `logging.rs:163-176`. Confirm `warn!` then `g.release()` without `settle_actual(0)`.
- **MANDATORY caller audit before shipping**: `rg 'settle_actual\(' crates/ | tee /tmp/settle_actual_callers.txt`. List in PR.

#### 8. 【如何验证 VERIFY】
1. `cargo test -p gw-ledger -- lease`: existing 12 tests pass; 2 new tests added.
2. Property test: 10 000 random Lease lifecycles (acquire, optional settle(N), drop) — sum of refunds == sum of (reserved - actual) for any path. Use `proptest`.
3. `actual_usage_credits` Err injection: a synthetic forced-Err in `logging.rs` does not leak; LEASE_POOL credits unchanged at end of request.

#### Mini-ADR
- Alternative A (chosen): add `settled: AtomicBool`, idempotent `settle_actual`, `release` triggers full-refund settle if no settle yet.
- Alternative B: leave fields alone; rewrite `Drop` to read `state.credits` and reverse-compute. Rejected: requires snapshot at acquire; race with concurrent acquires of same user.
- Alternative C: replace AtomicBools with one AtomicU8 state machine (Reserved → Settled → Released). Rejected: bigger rewrite, no new safety.

---

### TASK T17 — TaskFreeze sync-then-async release + DLQ (R5)

#### 1. 【是什么 What it IS】
- Edit `crates/gw-ledger/src/freeze.rs:TaskFreeze::release` (line 183) to NOT set `released=true` until the releaser has either succeeded OR exhausted retries (and a DLQ row has been persisted).

#### 2. 【不是什么 What it is NOT】
- NOT a change to `RequestFreeze` (already correct).

#### 3. 【为什么 Why this bug exists】
- R5: Current code sets `released=true` (line 184-189), THEN `tokio::spawn`s (line 198). If the releaser fails, the in-process counter is decremented (in the caller's view) but Redis stays inflated → ghost frozen credits.

#### 4. 【做什么 DO】
1. Make `release` `async fn try_release(&self) -> Result<(), TaskFreezeError>`; convert callers (use `task.rs:235`).
2. Inside, retry the releaser up to 3 times with `tokio::time::sleep` exponential backoff.
3. On exhaustion, write a row to `task_freeze_pending` PG table (T18) for sweeper retry.
4. `Drop` keeps best-effort spawn-and-warn for the case where caller didn't await.

#### 5. 【不做什么 DON'T】
- Do NOT block `Drop` on the releaser — that turns panics into deadlocks. Sync path is opt-in via `try_release`.

#### 6. 【怎么做 HOW (具体)】

```rust
impl TaskFreeze {
    pub async fn try_release(&self) -> Result<(), anyhow::Error> {
        if self.released.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let Some(releaser) = self.releaser.clone() else { return Ok(()); };
        let mut last_err = None;
        for attempt in 0..3 {
            match releaser.release(&self.uid, &self.task_id, self.freeze_amount).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    tracing::warn!(target: "gw_ledger", error=%e, attempt, "task freeze release failed; retrying");
                    last_err = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(100 << attempt)).await;
                }
            }
        }
        // Persist for sweeper (T18).
        if let Err(e) = task_freeze_pending::persist(&self.uid, &self.task_id, self.freeze_amount).await {
            tracing::error!(target: "gw_ledger", error=%e, "task freeze DLQ persist failed");
        }
        metrics::counter!("gw_task_freeze_release_failed_total").increment(1);
        Err(last_err.expect("retries failed"))
    }
}
```

Update `gw-proxy/src/filters/task.rs:235`:
```rust
contract.release_task_freeze(request.user_id, &task_id, frozen_micro).await?;
// becomes:
if let Some(freeze) = ctx.task_freeze.take() {
    if let Err(e) = freeze.try_release().await {
        tracing::error!(error=%e, "task freeze release failed permanently");
    }
}
```

#### 7. 【源码验证 SOURCE-CHECK】
- `freeze.rs:183-209`. Confirm CAS at 184-189 precedes `tokio::spawn` at 198. R5 confirmed.

#### 8. 【如何验证 VERIFY】
1. Inject 3 Redis blips at release-time; expect retry succeeds and `released=true`.
2. Inject permanent Redis failure; expect `released` flag stays `false`, DLQ row exists, error propagated.
3. `Drop` test: drop a TaskFreeze without `try_release`; old spawn-and-warn path still runs (best-effort).

---

### TASK T18 — task_freeze_pending PG table + sweeper (R9)

#### 1. 【是什么 What it IS】
- Rust migration `crates/gw-store/migrations/0012_task_freeze_pending.sql`.
- New crate file `crates/gw-ledger/src/freeze_sweeper.rs` exposing `pub async fn run(...)` that scans the table every 60 s and retries each row via the injected `TaskFreezeRelease`.
- Edit `crates/gw-proxy/src/filters/task.rs` so submit-time writes `(user_id, task_id, freeze_amount, created_at)` to `task_freeze_pending` BEFORE calling `contract.submit(...)`; on `submit` success, delete the row inside the SAME tx as freeze release; on `submit` fail, release immediately (also deleting the row).

#### 2. 【不是什么 What it is NOT】
- NOT a Go change. PG table lives in the Rust schema (gw-store).
- NOT a replacement of Redis frozen counter — the counter and PG row are complementary.

#### 3. 【为什么 Why this bug exists】
- R9: `task_id` is allocated as a fresh UUID at submit time; if Pingora dies between freeze and submit, the UUID is lost; no sweeper can find the orphaned freeze.

#### 4. 【做什么 DO】
1. Migration: create table.
2. submit-time insert + delete inside tx.
3. Sweeper service spawned via `supervised_spawn` (WS-2).

#### 5. 【不做什么 DON'T】
- Do NOT TTL the row — sweeper is the cleanup; TTL would silently expire ghost freezes.

#### 6. 【怎么做 HOW (具体)】

```sql
-- 0012_task_freeze_pending.sql
CREATE TABLE task_freeze_pending (
    user_id        text         NOT NULL,
    task_id        text         NOT NULL,
    freeze_amount  bigint       NOT NULL CHECK (freeze_amount >= 0),
    created_at     timestamptz  NOT NULL DEFAULT now(),
    last_attempt_at timestamptz NOT NULL DEFAULT now(),
    attempt        int          NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, task_id)
);
CREATE INDEX task_freeze_pending_due_idx ON task_freeze_pending (last_attempt_at);
```

`freeze_sweeper.rs::run`:

```rust
pub async fn run<R>(pool: PgPool, releaser: Arc<R>) -> anyhow::Result<()>
where R: TaskFreezeRelease + 'static {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        interval.tick().await;
        let rows = sqlx::query_as::<_, FreezePending>(
            "SELECT user_id, task_id, freeze_amount FROM task_freeze_pending
             WHERE last_attempt_at < now() - interval '1 minute'
             ORDER BY last_attempt_at LIMIT 100 FOR UPDATE SKIP LOCKED")
            .fetch_all(&pool).await?;
        for r in rows {
            match releaser.release(&UserId::from(r.user_id.as_str()), &r.task_id, r.freeze_amount as u64).await {
                Ok(_) => {
                    let _ = sqlx::query("DELETE FROM task_freeze_pending WHERE user_id=$1 AND task_id=$2")
                        .bind(&r.user_id).bind(&r.task_id).execute(&pool).await;
                }
                Err(_) => {
                    let _ = sqlx::query("UPDATE task_freeze_pending SET attempt=attempt+1, last_attempt_at=now() WHERE user_id=$1 AND task_id=$2")
                        .bind(&r.user_id).bind(&r.task_id).execute(&pool).await;
                }
            }
        }
    }
}
```

In `task.rs:submit` flow, persist the pending row before `contract.submit(...)`; delete it on successful submit; release+delete on submit fail.

#### 7. 【源码验证 SOURCE-CHECK】
- `gw-proxy/src/filters/task.rs:235`. Confirm direct release call (no PG-backed pending row today).

#### 8. 【如何验证 VERIFY】
1. Chaos: kill Pingora between freeze and submit; restart; sweeper finds the orphan within ≤ 60 s and releases.
2. `task_freeze_pending` is empty after a healthy submit.

---

### TASK T19 — Atomic Lua (XADD ; HSET state=settled ; DEL inflight) + state-aware reconciler (Billing-2/3 / R6 / REV-FINAL-M7/M8)

#### 1. 【是什么 What it IS】
- New Lua script in `crates/gw-task/src/scripts.rs` (or `gw-ledger/src/scripts.rs` if more appropriate; pick the crate that owns flusher) named `SETTLE_ATOMIC_LUA`.
  - KEYS: `[1]=stream usage:records, [2]=inflight hash key`.
  - ARGV: stream entry fields + `state` value `"settled"`.
  - Logic: `local cur = redis.call('HGET', KEYS[2], 'state'); if cur == 'settled' then return 'noop' end; redis.call('XADD', KEYS[1], '*', ...args); redis.call('HSET', KEYS[2], 'state', 'settled'); redis.call('DEL', KEYS[2]); return 'ok'`.
  - REV-FINAL-M7: do NOT include `MAXLEN` in XADD; trim runs in a separate sweeper. Server-side error inside Lua interrupts the script with no rollback — keeping XADD trim out of Lua avoids that class.
- Edit `flusher.rs:flush` (around line 907) to call this Lua per LedgerItem instead of the current pipeline pattern.
- Edit `inflight_reconciler.rs:build_fallback_record` (line 76) to read `state` field; if `state == "settled"`, DEL the key and skip emission. REV-FINAL-M8 supersedes by ALSO checking the usage:records stream over last 24 h via `XRANGE` for `(rid, attempt)` — if found, mark and skip.

#### 2. 【不是什么 What it is NOT】
- NOT a change to XACK semantics — Lua does not touch consumer groups.
- NOT a removal of inflight_reconciler's run path — its trigger remains, but its fallback emission is now guarded by the state check.

#### 3. 【为什么 Why this bug exists】
- Billing-2: pipeline RTT partial-failure double-charges.
- Billing-3 / R6: DEL failure after XADD success leaves the key in `state=settled`; reconciler re-emits CrashFallback for an already-charged request.

#### 4. 【做什么 DO】
1. Write SETTLE_ATOMIC_LUA per HOW.
2. Replace the per-item pipeline in `flush` with `redis.eval(SETTLE_ATOMIC_LUA, ...)`.
3. Per REV-FINAL-M7, ensure XADD has no MAXLEN (move trim to a sweeper task; document in WS-9).
4. Per REV-FINAL-M8, in reconciler: BEFORE emitting CrashFallback, query `usage:records` via `XRANGE - +` filtered to last 24 h for matching `(request_id, attempt_index)`; if present, DEL the inflight key and skip emission.
5. Alternatively / additionally, keep state-field check as a fast path (no XRANGE if `state == "settled"`).
6. INVESTIGATE-008 (v2 §99): if production runs Redis Cluster, KEYS[1]=stream and KEYS[2]=hash must share a slot. Audit: are they routed to the same slot today? If yes, document; if no, add hash-tag (`usage:records` → `usage:{<rid>}:records`, `billing:inflight:{<rid>}:<attempt>`). Decide BEFORE this lands.

#### 5. 【不做什么 DON'T】
- Do NOT batch XADDs inside Lua across distinct keys (CROSSSLOT risk).
- Do NOT add MAXLEN in Lua.
- Do NOT trust DEL to succeed if XADD did — model both as separate atomic ops at the Redis level; the Lua wraps them and rollback semantic is "Lua all-or-nothing" within one slot.

#### 6. 【怎么做 HOW (具体)】

```rust
const SETTLE_ATOMIC_LUA: &str = r#"
local cur = redis.call('HGET', KEYS[2], 'state')
if cur == 'settled' then return 'noop' end
redis.call('XADD', KEYS[1], '*', unpack(ARGV))
redis.call('HSET', KEYS[2], 'state', 'settled')
redis.call('DEL', KEYS[2])
return 'ok'
"#;
```

Inside `flush(...)`:

```rust
for item in taken.iter() {
    let args = build_xadd_args(&item.record);
    let inflight_key = format!("billing:inflight:{}:{}:{}",
        worker_instance, item.record.request_id, item.record.attempt_index);
    let res: String = client
        .eval(SETTLE_ATOMIC_LUA, vec!["usage:records".into(), inflight_key], args)
        .await?;
    match res.as_str() {
        "ok" | "noop" => {}
        other => warn!(?other, "settle_atomic_lua returned unexpected value"),
    }
}
```

Inside `inflight_reconciler::build_fallback_record`, after the hash read:

```rust
if hash.get("state").map(|s| s.as_str()) == Some("settled") {
    let _: () = pool.del::<(), _>(&[key]).await.ok().unwrap_or(());
    return Ok(None);
}
```

And in `run`, before emit, REV-FINAL-M8:
```rust
let already_settled = scan_usage_records(pool, &rec.request_id, rec.attempt_index, Duration::from_secs(86400)).await?;
if already_settled {
    let _ = pool.del::<(), _>(&[key.as_str()]).await;
    continue;
}
```

#### 7. 【源码验证 SOURCE-CHECK】
- `flusher.rs:907` (the `flush` fn). Read entire `flush`. Confirm current pipeline pattern.
- `inflight_reconciler.rs:54`. Confirm `pool.del::<(), _>(&[key_str.as_str()]).await?` swallows DEL failure into propagated error (R6 confirmed).
- `inflight_reconciler.rs:84-95` (state field parsing). Confirm hash extraction ignores `state` (Billing-3 confirmed).

#### 8. 【如何验证 VERIFY】
1. `tests/e2e/chaos/ws1/del_failure_test.rs`: simulate Lua-level success but background DEL fail; expect no duplicate UsageRecord in stream after reconciler runs.
2. Property: 10 000 settle ops with random Redis blips; final stream length == unique (rid, attempt_index).

---

### TASK T20 — parse_response Err → 502 (lease portion of R7)

#### 1. 【是什么 What it IS】
- Hand-off task. T16 already plumbs the LeaseGuard refund + Terminated UsageRecord. This task documents the contract with WS-6:
  - When `parse_response` returns Err (protected-mode cross-protocol non-stream), `g.release()` runs (full refund via T16's coupling) AND `emit_usage_record_terminated` runs (T16) AND HTTP returns 502 `GW_UPSTREAM_PROTOCOL_DECODE_ERROR` (owned by WS-6 / `response_filter`).

#### 2. 【不是什么 What it is NOT】
- NOT the HTTP-status fix itself. That lives in WS-6's sub-plan.

#### 4. 【做什么 DO】
- Coordinate with WS-6 owner; ensure the WS-6 sub-plan task that handles `Proxy-7 / R24` includes `GW_UPSTREAM_PROTOCOL_DECODE_ERROR` mapping.

#### 8. 【如何验证 VERIFY】
- Verified in WS-6 sub-plan; cross-link this task ID there.

---

### TASK T21 — replay_file per-envelope SETNX dedup (Billing-4 / REV-FINAL-M9 partial)

#### 1. 【是什么 What it IS】
- Edit `crates/gw-ledger/src/backlog.rs:replay_file` (around line 229-252).
  - Before rename to `.replaying`, the procedure already does (read-only inspection — Read this file in full first).
  - **Add**: for each envelope read, compute `let hash = sha256(envelope.bytes)`; call `SETNX redis:replay_dedup:{hash} 1 EX 604800`; if `0` (already present), skip pushing; if `1`, push and continue.
  - Replace `let _ = tokio::fs::remove_file(path).await` with explicit error handling: log and escalate (counter `gw_billing_backlog_remove_failed_total`); per REV-FINAL-M9 do NOT silently swallow.

#### 4. 【做什么 DO】
1. Add SETNX gating.
2. Surface remove_file failures.

#### 5. 【不做什么 DON'T】
- Do NOT remove the rename-to-`.replaying` step — it's how we know a file is in flight.

#### 6. 【怎么做 HOW (具体)】

```rust
let key = format!("redis:replay_dedup:{}", hex::encode(Sha256::digest(envelope_bytes)));
let already: bool = pool.set(&key, "1", Some(Expiration::EX(604800)), Some(SetOptions::NX), false).await? == None;
if already {
    metrics::counter!("gw_billing_replay_dedup_hit_total").increment(1);
    continue;
}
backlog.push(env).await;
```

After the loop:
```rust
if let Err(e) = tokio::fs::remove_file(path).await {
    metrics::counter!("gw_billing_backlog_remove_failed_total").increment(1);
    tracing::error!(error=%e, path=?path, "backlog file remove failed");
}
```

#### 7. 【源码验证 SOURCE-CHECK】
- `backlog.rs:229` (`let _ = tokio::fs::rename(&p, &dest).await`) and `:252` (`let _ = tokio::fs::remove_file(path).await`). Confirm both swallow errors. Billing-4 confirmed.

#### 8. 【如何验证 VERIFY】
1. Chaos: kill between rename and remove; restart; same file re-found, but SETNX prevents duplicate emission; metric `gw_billing_replay_dedup_hit_total > 0`.

---

### TASK T22 — catch_unwind spill-to-disk (Billing-8 / REV-FINAL-M9)

#### 1. 【是什么 What it IS】
- Edit `crates/gw-ledger/src/flusher.rs:flush` (line 907) and the three `catch_unwind` callers (lines 207, 222, 234, 411, 432).
- Wrap `flush(...)` so that on `Err(panic)`, the `taken` batch is written to a spill file in `SpillRoot` BEFORE the panic is logged. On next start, `replay_file` (T21) picks it up. Use `Semaphore::new(32)` to bound concurrent spills.

#### 4. 【做什么 DO】
1. Before `mem::take` (line 205), wrap `taken` in `Arc<Mutex<...>>` accessible to a spill handler.
2. Actually: the simpler shape is to spill INSIDE the `catch_unwind` Err branch:
   ```rust
   if let Err(panic) = result {
       let _ = spill_to_disk(&taken, &spill_root).await;
       // ... existing metric ...
   }
   ```
   This is safe because `taken` is moved by `mem::take` BEFORE `flush` runs; on panic, `taken` lives in the surrounding scope. Actually, in current code `taken` is moved INTO `flush`. The Edit must restructure so `taken` stays in the caller and `flush` borrows it (`&mut Vec<LedgerItem>`).
3. Add the semaphore to bound concurrent spills.

#### 5. 【不做什么 DON'T】
- Do NOT clone `taken` before flush — that's the v1 plan rejected by REV-FINAL-M9 (doubles memory).
- Do NOT silently swallow spill errors — if disk write fails, **escalate**: log + metric + force the supervisor to restart.

#### 6. 【怎么做 HOW (具体)】

```rust
let spill_sem = Arc::new(Semaphore::new(32));

// inside the catch_unwind err branch:
if let Err(_panic) = result {
    let permit = spill_sem.clone().acquire_owned().await; // may block, by design
    let snapshot_path = match spill_batch_to_disk(&spill_root, &taken).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error=%e, "flusher panic spill FAILED — escalating");
            metrics::counter!("gw_billing_flusher_spill_failed_total").increment(1);
            panic!("flusher panic + spill failed; supervisor restart needed");
        }
    };
    metrics::counter!("gw_billing_flusher_panic_total").increment(1);
    tracing::error!(?snapshot_path, "flusher panicked; batch spilled to disk for replay");
    drop(permit);
}
```

`spill_batch_to_disk` writes the same JSONL envelope shape `replay_file` understands.

#### 7. 【源码验证 SOURCE-CHECK】
- `flusher.rs:205-216`. Confirm `let mut taken = std::mem::take(&mut buf)` and `catch_unwind` then drop. Billing-8 confirmed.

#### 8. 【如何验证 VERIFY】
1. Inject `panic!()` into `flush`; observe spill file present; restart; observe `replay_file` picks it up; usage:records contains the records.

---

### TASK T23 — Go provenanceRank add `cli_extracted` (C2)

#### 1. 【是什么 What it IS】
- Edit `modules/billing/business/dedup.go` lines 18-25.

#### 4. 【做什么 DO】
Use REV-FINAL-H8.2 spaced layout:
```go
var provenanceRank = map[string]int{
    "upstream_final":   10,
    "cli_extracted":    8,    // NEW
    "upstream_partial": 6,
    "estimated":        4,
    "crash_fallback":   2,
    "unknown":          1,
    "":                 0,
}
```
- Bump the persisted value format from `v2:` to `v3:` (or add tolerance so legacy `v2:` is read in the dual-window). REV-FINAL-H8.2 reads "存储值带版本 `v3:<rank>:<cost>`".

#### 5. 【不做什么 DON'T】
- Do NOT remove `v2:` parsing — the Redis state from before this PR uses `v2:`.

#### 6. 【怎么做 HOW (具体)】
- Update `provenanceRank` per above.
- Update `claimCostScript` Lua to also accept `v3:` and write `v3:` (read both shapes in `parse_stored`; write `v3:`).

#### 7. 【源码验证 SOURCE-CHECK】
- `dedup.go:18-25`. Confirm no `cli_extracted`.
- `dedup.go:320` (`return "v2:" + strings.Join(...)`) — confirm v2 prefix.
- `dedup.go:455-582` (Lua script) — confirm `parse_stored` handles `v2:` only.

#### 8. 【如何验证 VERIFY】
1. Test new tag wins over `estimated`.
2. Test: pre-existing `v2:` Redis values still parse.

---

### TASK T24 — dedupKey + attempt_index (C4 / REV-FINAL-H8.7/H8.9)

#### 1. 【是什么 What it IS】
- Edit `dedup.go:dedupKey` (line 56) to take both `requestID` and `attemptIndex int32` and return `"settled:" + requestID + ":" + strconv.Itoa(int(attemptIndex))`.
- Update all callers; add legacy fallback `settled:<rid>` for read paths during the deploy window.
- Per REV-FINAL-H8.9: **Rust deploys FIRST** (Rust starts emitting attempt_index). Go must read both shapes for one release window. (This corrects v1's "Go first" guidance — confirmed by REV-FINAL-H8.9.)

#### 2. 【不是什么 What it is NOT】
- NOT a Lua-script-only change — the Go-side key construction and read paths both move.

#### 3. 【为什么 Why this bug exists】
- C4: production has already observed retried-attempt billing being deduped by the first attempt's key, dropping real settlements.

#### 4. 【做什么 DO】
1. Change signature `dedupKey(requestID string, attemptIndex int32) string`.
2. All call sites pass `attempt_index` (which Rust will start emitting per WS-8 schema bump).
3. For read paths, also try the legacy key `"settled:"+rid`.
4. CI gate: `scripts/check-deploy-order.sh` from WS-12 enforces Go reads-both-shapes when Rust may be either version.

#### 5. 【不做什么 DON'T】
- Do NOT delete legacy reads until WS-12 deploy-order gate verifies the Rust min version supports attempt_index.

#### 6. 【怎么做 HOW (具体)】

```go
func dedupKey(requestID string, attemptIndex int32) string {
    return fmt.Sprintf("settled:%s:%d", requestID, attemptIndex)
}

// legacyDedupKey kept for read-only fallback during cutover.
func legacyDedupKey(requestID string) string { return "settled:" + requestID }

// On read:
key := dedupKey(rid, attempt)
exists, _ := rdb.Exists(ctx, key).Result()
if exists == 0 {
    key = legacyDedupKey(rid)  // graceful read-fallback for pre-attempt records
}
```

#### 7. 【源码验证 SOURCE-CHECK】
- `dedup.go:56`. Confirm current is `"settled:" + requestID`. C4 confirmed.

#### 8. 【如何验证 VERIFY】
1. Mixed-version test: write with attempt=0 under legacy shape, read back with attempt=0 under new shape — succeed via fallback.
2. New attempts (attempt > 0) write to new shape; old reads cannot find them — by design.

---

### TASK T25 — Terminated semantic (C3)

#### 1. 【是什么 What it IS】
- Two coordinated edits:
  - Rust (already in T16): emit `UsageRecord { vendor_exposure_state: Terminated, credits_consumed: Some(0) }` on Err / CLI early-return.
  - Go: in `settler`, force `cost_micros = 0` whenever the incoming UsageRecord has `vendor_exposure_state == "Terminated"` AND `credits_consumed.IsSome() && credits_consumed == 0`.

#### 4. 【做什么 DO】
1. Verify T16 has emitted the Rust side.
2. Find the settler entry: `grep -rn 'VendorExposureState' modules/billing/ shared/rustpact/`. Add the force-zero branch.

#### 5. 【不做什么 DON'T】
- Do NOT force zero based on Terminated alone — it must also have `credits_consumed: Some(0)`. Otherwise a partial-execution Terminated would be under-billed.

#### 6. 【怎么做 HOW (具体)】

```go
// In settler, after extracting UsageRecord:
if rec.VendorExposureState == "Terminated" && rec.CreditsConsumed != nil && *rec.CreditsConsumed == 0 {
    costMicros = 0
}
```

#### 8. 【如何验证 VERIFY】
1. e2e: protected-mode CLI early-return → wallet untouched; UsageRecord emitted with `Terminated/0`.

---

### TASK T26 — wallet:idem dual-write migration script + cutover (C7)

#### 1. 【是什么 What it IS】
- A migration window during which `Reserve` SETNX and `Commit` INSERT both run for each `op`/`key`. Lasts ≥ 25 h (one TTL window of 24 h + safety margin).
- A monitoring counter `wallet_idem_dual_write_inconsistency_total` that increments when SETNX returns `false` (Redis says exists) AND `Commit` returns `didWrite=true` (PG says new), OR vice-versa.
- After the window, a flag flip removes the SETNX path.

#### 4. 【做什么 DO】
1. Add `dual-write` mode flag in `Idem` (`DualWrite bool`).
2. In Deduct/Credit/Refund/adminDebit/RecordUsage: call both Reserve and Commit during the window; alert on disagreement.
3. After 25 h confirm zero inconsistency, ship a PR that turns off dual-write and removes `Reserve` calls.

#### 5. 【不做什么 DON'T】
- Do NOT close the window before reaching 25 h actual production time.
- Do NOT swallow Redis errors during dual-write — alert.

#### 6. 【怎么做 HOW (具体)】
- See HOW in T03-T06. Each switches to call **both** during the window; the comparison logic is centralized in `idem.go`.

#### 8. 【如何验证 VERIFY】
1. Soak: 25 h with `wallet_idem_dual_write_inconsistency_total == 0` is the cutover gate.

---

### TASK T_BACKFILL_PAYMENT — Backfill v1-era abandoned payments (REV-009 / meta §H)

#### 1. 【是什么 What it IS】
- One-shot `scripts/payments-backfill-outbox.sh` that scans `payment_orders` where `status = 'succeeded' AND credited_at = 0 AND created_at < <cutoff>` and:
  - Either (a) writes a row to `payment_outbox` (status='pending') to be processed normally, OR
  - (b) marks the order `credited_at = X` and inserts a manual audit row indicating ledger reconciliation.

#### 4. 【做什么 DO】
1. List candidate rows (DRY-RUN).
2. Spot-check 10 rows against Redis `wallet:idem:credit:po_credit:<id>` presence.
3. Run on a maintenance window.

#### 5. 【不做什么 DON'T】
- Do NOT bulk-credit without manually inspecting the candidate set first — the count must reconcile against the v1-era webhook logs.

#### 8. 【如何验证 VERIFY】
- Each backfilled order has a corresponding `idempotency_keys` row, `wallet_transactions` row, and `payment_outbox.status='committed'`.

---

### TASK T_TEST — Chaos suite (depends on WS-13)

#### 1. 【是什么 What it IS】
- New tests under `tests/e2e/chaos/ws1/`:
  - `db_blip_test.go` (T03 / T04 / T05 / T06)
  - `redis_blip_test.go` (T08 / T09)
  - `pingora_kill_test.go` (T17 / T18 / T19 / T22)
  - `mixed_deploy_test.go` (T24)
  - `concurrent_admin_test.go` (T07 / T09 / T13)

#### 4. 【做什么 DO】
- For each scenario, declare PASS/FAIL JSON in `.chaos_report/<scenario>.json` per WS-13 contract.

#### 8. 【如何验证 VERIFY】
- `make chaos-ws1` is green; uploaded to CI dashboard.

---

## 4. Cross-WS Hand-offs

| Task | Hand-off |
|---|---|
| T16 → WS-6 | parse_response Err HTTP status (T20). |
| T19 → WS-2 | XACK semantics + reconciler ownership stay in WS-1; WS-2 owns supervisor that runs them. |
| T20 → WS-6 | Owns `response_filter` rewrite. |
| T22, T17, T18 → WS-4 | supervisor (`supervised_spawn` / `Supervised`) is owned by WS-4; WS-1 consumes the API. |
| T23, T24, T25 → WS-8 | rustpact schema/version (`_pv`). T23-25 must land WITH the `_pv` bump WS-8 ships. |
| All T → WS-14 | Goose numbering. |
| T07 → WS-11 | Frontend nonce generation. |
| T14 → WS-5 | mgmttoken error propagation is shared. |

---

## 5. DoD (WS-1)

1. Every task in §3 has a merged PR with the 8-section block in the description plus a mini-ADR.
2. `make chaos-ws1` (T_TEST) green on staging.
3. 24-h soak with `wallet_idem_dual_write_inconsistency_total == 0` for T26 cutover gate.
4. `tests/e2e/wallet/concurrent_deduct_test.go` — 1 000 parallel × 60 s × DB blip at request 500 — `wallets.recharge_balance_micros + gift_balance_micros` delta == Σ HTTP 200 cost ± 0.
5. v2 §99 INVESTIGATE-004, -005, -006, -008 each have a written-up resolution in `docs/issues/2026-06-17-investigates/`.
6. `scripts/check-deploy-order.sh` (WS-12) blocks any deploy where Rust attempt_index emitter precedes Go reader.
7. All 22 finding IDs in §2 mapped, none in `open` state.

---

## 6. Open INVESTIGATE items in WS-1 Scope

| ID | What's open | Owner | Blocking? |
|---|---|---|---|
| INVESTIGATE-004 | `wallet.Credit` op-namespace collision audit (T04) | Track A1 | Yes — must resolve before T04 lands. |
| INVESTIGATE-005 | TaskFreeze sweeper cross-process coordination (T17/T18) | Track B | Yes — affects T18 table schema. |
| INVESTIGATE-006 | Does `shared/contracts` expose tx-receiving Purchase? (T13) | Track A2 | Yes — alternative-A vs outbox choice. |
| INVESTIGATE-008 | Single-shard vs Cluster Redis for SETTLE_ATOMIC_LUA (T19) | Track B | Yes — slot-routing risk. |
| INVESTIGATE-011 | Spot-check 10 audit "file:line" claims in WS-1 scope | All WS-1 owners | No — but ship list in PR descriptions. |

---

## 7. Deviations from v2 (with rationale)

| Plan area | v2 said | This sub-plan says | Why |
|---|---|---|---|
| T01 PK | `(op, key)` | `(op, key, created_at)` | Postgres requires partition key in PK for RANGE partitioning by `created_at`; `(op, key)` uniqueness is enforced via the auto-created per-partition unique index. v2 didn't address this. |
| T15 commit timestamp | `pg_xact_commit_timestamp` | `clock_timestamp()` at INSERT (with `pg_xact_commit_timestamp` as alternative) | Requires `track_commit_timestamp = on` in prod; this is not guaranteed. The plan documents the prerequisite check and falls back. |
| T24 deploy order | v1 master plan §6.1 / S1.4 said "Go first" | Rust first (per REV-FINAL-H8.9) | v2 §4 REV-FINAL-H8.9 explicitly inverts this; corrected here. |
| T16 LeaseGuard impl | v1 said "Drop check settled" | Candidate A: add `settled: AtomicBool`, release triggers refund-on-Drop coupling | REV-FINAL-M3 lists 3 candidates; sub-plan picks A and documents the caller-audit prerequisite. |
| T19 (Lua trim) | v1/master implied XADD MAXLEN inside Lua | No MAXLEN in Lua | REV-FINAL-M7 — server-side error inside Lua interrupts the script with no rollback; trim is moved to a separate sweeper. |
| T19 reconciler | v1 had a separate `billing:settled:{rid}:{attempt}` sentinel set | Reconciler uses `XRANGE usage:records` over last 24 h | REV-FINAL-M8 — independent sentinel has unclose-able time gaps. |

---

— end of WS-1 sub-plan —
