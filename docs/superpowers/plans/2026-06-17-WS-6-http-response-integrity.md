# WS-6 HTTP Response Integrity — TDD Sub-Plan

> **Mirrors**: `AILinkMax-Go/docs/superpowers/plans/2026-06-17-WS-6-http-response-integrity.md` and `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-6-http-response-integrity.md` (byte-identical; update both in the same commit).
>
> **Parents**:
> - v1: `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-6, §5
> - v2: `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §0 (8-section format), §4 WS-6 REV-FINAL-H6.1..H6.7
> - Original audits: Proxy-5, Proxy-6, Proxy-7, Proxy-9, Proxy-10 (Rust) + new findings R7, R8, R24
>
> **Scope coverage**: Proxy-5, Proxy-6, Proxy-7, Proxy-9, Proxy-10, R7, R8, R24, REV-FINAL-H6.1..H6.7 (Proxy-8 also lives in WS-3 — coordinate via §0.4 below).
>
> **Format**: every task uses the v2 §0 8-section template — `IS / NOT / WHY / DO / DON'T / HOW / VERIFY / Dependencies + Adversarial review`. Skipping a section blocks merge.
>
> **Audience**: this plan assumes **no Rust or Go expertise**. Every `HOW` block is concrete file:line + literal command. No "see code", "as needed", "implement appropriately".

---

## 0. Read-me first (mandatory for the executor)

### 0.1 v2 §0 重要 1 — process anchors

Before any TDD step you MUST:

1. **Open the cited file:line with the `Read` tool** and visually confirm the code matches what this plan claims. If the code has drifted (renames, refactors, new fields), record the drift at the top of the task entry and ADJUST the task before writing the test. Source code wins over this document.
2. For each REV-FINAL-* in this WS-6 plan, this sub-plan already lists **at least one alternative candidate** plus a short ADR-style "why we chose A over B". You may swap candidates if you find the source-of-truth code rules one out, but you must update the ADR block.
3. Treat any `INVESTIGATE-*` reference as an open item — do not assume the conclusion.

### 0.2 v2 §0 重要 2 — sonnet output is HYPOTHESIS

The "FINAL-H6.1 preflight cannot compose with streaming" framing in v2 is **architecturally correct but partly overstated**. Source verification (2026-06-17, this plan author) showed:

| Claim | Verified state in `gateway.rs` HEAD |
|---|---|
| `response_filter` commits header to client before body can be inspected | TRUE — `response_filter` (gateway.rs:1070-1368) records snapshots and runs failover/error-classification but does NOT buffer body. The body-side processing happens in `response_body_filter` (gateway.rs:1453-1580). Once `response_body_filter` returns `Ok(())` with body bytes, the chunk has been handed to Pingora's downstream writer. |
| For streaming, once the first body byte is committed, status cannot be retracted | TRUE — `stream_probe::mark_client_committed` (gateway.rs:1568) flips `ctx.client_body_committed` only after a body chunk has been written, but Pingora has already sent headers + the chunk. Any failure after that point is a stream-level error, not an HTTP status flip. |
| For non-stream cross-protocol, body is held in `upstream_body_accum` and re-rendered at EOS | TRUE — `upstream_body_accum` (ctx.rs:532, ctx.rs:698 init) is allocated by `filters::response::handle_non_stream_chunk` and parsed once at EOS. The full body is buffered server-side before any byte goes downstream. |

Implication: REV-FINAL-H6.1's "split into 3 probe modes" is the right shape, but the **non-stream cross-protocol case is the only one where a server-buffered preflight buffer is implementable**; the streaming case must rely on the existing `stream_probe` and emit SSE error frames for late failures. This sub-plan codifies that distinction.

### 0.3 v2 §0 重要 2 — R8 reset matrix verified

The R8 critique — `reset_response_attempt_state` (gateway.rs:131-153) leaks attempt-scoped state across response-side retries — is **fully verified**. The function currently clears 13 fields:

```
inflight_guard, picked_vendor_key, picked_vendor, upstream_codec, upstream_status,
body_started, client_body_committed, stream_phase, probe_started_at, probe_buffer,
pending_probe_failure, stream_probe_retry_pending, sse_leftover, pending_client_body,
archive_upstream_response_body, archive_downstream_response_body
```

It does NOT clear (full audit done against `ctx.rs:291-576`):

| Field (ctx.rs line) | Leak class on retry | What goes wrong |
|---|---|---|
| `upstream_body_accum` (532) | Frankenstein body | retry concatenates new bytes onto failed-attempt bytes; codec parse explodes or returns garbage |
| `operation_id` (526) | LRO misattribution | Gemini long-running id from prior attempt leaks into next attempt's UsageRecord |
| `quota_snapshot` (568) | Wrong-vendor quota | quota header from failed key attached to record for successful key |
| `stream_state` (409) | Codec accumulator desync | tool-call argument deltas / thinking signatures from prior attempt pollute next parse |
| `usage` (UsageAccumulator) (351) | Double-count tokens | next attempt's `Usage` event additively combines with stale counters |
| `usage_provenance` (363) | Provenance mis-rank | retry's `cli_extracted=4` would be overwritten by stale `upstream_partial=6` from prior attempt |
| `resp_content_encoding` (476) | Decode wrong codec | new upstream may use `identity`, prior was `gzip`; the field is in fact re-set in `response_filter` so this is safe — annotate accordingly |
| `archive_upstream_response_meta` (566) | Wrong headers in archive | response_filter overwrites this so it self-heals, but if next attempt fails before response_filter, prior meta lingers — partial leak |
| `archive_downstream_response_meta` (571) | Wrong headers in archive | written at EOS in response_body_filter; if EOS never reached on retry, prior stays |
| `error_outcome` (458) | Stale classification | next attempt may be unaffected but logging filter consults `error_outcome.last` |
| `error_passthrough_body`, `error_passthrough_active` (463-464) | Stale passthrough body | could be written over response of successful retry |
| `cancel_grace_active` (369) | Phantom cancel | next attempt sees `Active`, misroutes through grace path |
| `egress_sent_at` (520) | Wrong RTT | next attempt observes prior dispatch instant; histogram poisoned |
| `credits_consumed`, `coef_table_version_at_settle` (384, 387) | Settle double-count | settler reads stale `Some(n)` and refunds wrong amount |
| `upstream_error_code`, `upstream_error_detail` (439, 444) | Wrong logging code | retry succeeds, but error_code still set → logging filter emits success+error |
| `pending_probe_failure` already cleared (143); double-check `stream_probe_retry_pending` already cleared (143) — these are fine |

This sub-plan task **W6-T03** enumerates the FULL ReqCtx field matrix and classifies every field into one of four buckets: `persist`, `attempt-reset`, `request-end-reset`, `protocol-only` (= filter-internal, never crosses an attempt).

### 0.4 Cross-WS coordination (REV-003c)

`crates/gw-proxy/src/filters/response.rs` is also touched by WS-3 (buffer caps) and WS-8 (codec re-render). Per v2 §REV-003c, **WS-3 and WS-6 must be merged into a single PR by Track C** (Senior Rust #2). WS-8 codec changes ship in a separate PR. The order inside the merged PR is: WS-3 cap traits → WS-6 H6.1 non-stream preflight (uses cap traits) → WS-6 H6.3 reset matrix → WS-6 H6.4 archive channel hardening → WS-6 H6.5 spawn pools → WS-6 H6.6 block_in_place removal → WS-6 H6.2 grace-expiry emit → WS-6 H6.7 e2e test.

### 0.5 Cross-WS coordination with WS-1

Tasks W6-T08 (R7 + R24 502 path) and WS-1 task T17 (Rust `parse_response` Err arm) ship as one PR per v1 §6.1. WS-1 T17 owns the lease/billing side; WS-6 W6-T08 owns the HTTP status + error-code wiring. The shared error code `GW_UPSTREAM_PROTOCOL_DECODE_ERROR` is added to `crates/gw-core/src/error/codes.rs` by **whichever PR lands first**; the second PR references it.

### 0.6 INVESTIGATE-007 (carried from v2 §99)

> **INVESTIGATE-007**: Proxy-9 `block_in_place` replacement consumes Tokio runtime worker slots; verify worker count is sufficient.

Resolution path inside this WS-6 plan: task **W6-T07** includes a 60-second `make e2e-soak-smoke` with `tokio_unstable` runtime metrics enabled, then asserts `tokio_runtime_workers_busy_ratio_p99 < 0.85`. If the gate fails the executor must escalate to bump `[server] proxy_runtime_workers` before merging the PR.

---

## 1. Root-cause + finding-ID coverage table

| Finding | Root cause (one line) | v2 REV-FINAL ref | Task(s) below |
|---|---|---|---|
| **Proxy-5** | `tokio::spawn` of emit / counter / quota lacks timeout & concurrency cap; backlog explodes under slow Redis | REV-FINAL-H6.5 | W6-T05 |
| **Proxy-6** | Cancel-grace `Expired` branch returns `Err` without flushing archive surfaces → detailed-log gap | REV-FINAL-H6.2 | W6-T01 |
| **Proxy-7** | response_filter cannot inspect body; status committed before parse failure detectable | REV-FINAL-H6.1 (3-mode split) | W6-T02 |
| **Proxy-8** | Non-stream `upstream_body_accum` lacks hard cap → OOM via malicious upstream | (also WS-3 cap) | W6-T02 sub-step C (cap hookup) |
| **Proxy-9** | 4× `block_in_place` inside `response_body_filter` blocks Tokio worker | REV-FINAL-H6.6 | W6-T07 |
| **Proxy-10** | `tokio::spawn` of archive surfaces unbounded → memory + FD exhaustion under Redis stall | REV-FINAL-H6.4 / H6.5 | W6-T04, W6-T05 |
| **R7** | Protected-mode parse failure returns `Ok(0)` + HTTP 200 + empty body → free request | REV-FINAL-H6.1 + WS-1 T17 | W6-T08 (HTTP); WS-1 T17 (lease) |
| **R8** | `reset_response_attempt_state` field matrix incomplete → Frankenstein body on retry | REV-FINAL-H6.3 | W6-T03 |
| **R24** | Cross-protocol non-stream `parse_response` failure ships raw upstream bytes verbatim to client | REV-FINAL-H6.1 + W6-T08 | W6-T02 + W6-T08 |
| **REV-FINAL-H6.7** | No cross-protocol mid-stream switch byte-fidelity test | — | W6-T09 |

---

## 2. Architecture decisions (one ADR per H6.x)

> Each ADR follows: **Context → Options A/B (or A/B/C) → Choice + Why**. Update if source verification rules out the choice.

### ADR-W6-01 — REV-FINAL-H6.1 preflight buffer (Proxy-7 / R24 / R7-HTTP)

**Context.** `response_filter` (gateway.rs:1070) runs **before any body byte is forwarded**, but it has no access to the body — only headers + status. The body is processed in `response_body_filter` (gateway.rs:1453) chunk-by-chunk. For streaming traffic, Pingora forwards each chunk to the client as soon as `response_body_filter` returns. For non-stream cross-protocol traffic, `filters::response::handle_non_stream_chunk` accumulates into `ctx.upstream_body_accum` (ctx.rs:532) and only re-renders at EOS. v1's S6.1 proposed a single preflight buffer that delays header commit until probe/parse passes; v2's REV-FINAL-H6.1 split into three modes.

**Options.**
- **A. Three-mode split (v2 candidate).**
  - *Header-only* (egress class set, status ≥ 400, status < 200): no body inspection needed; decide entirely in `response_filter`. May return `Err` to trigger failover.
  - *Stream-probe first-N* (existing `stream_probe`): unchanged — first chunk is probed; commit once probe passes; late failure emits an SSE `error` event into the in-flight stream (does NOT flip status).
  - *Non-stream cross-protocol parse*: server-side buffer in `upstream_body_accum`, capped at `[proxy.upstream_body_accum_cap_bytes]` (default 8 MiB, shared with Proxy-8 WS-3 work); on cap → `502 GW_UPSTREAM_BODY_TRUNCATED`; on parse failure → `502 GW_UPSTREAM_PROTOCOL_DECODE_ERROR`. Header commit is deferred for this path only.
- **B. Universal preflight (v1 S6.1).** Buffer every response into RAM before commit. Rejected: streaming can be megabytes-per-second indefinitely; buffering breaks SSE timing guarantees and OOMs the gateway. Verified by source: `response_body_filter` already streams chunk-at-a-time; trying to buffer would require re-architecting the filter to swallow all chunks until EOS, which Pingora's API does not support for streaming.
- **C. Probe-only universal (no non-stream buffer).** Treat all traffic as streaming, no parse-fail recovery on non-stream. Rejected: R24 (verbatim wrong-protocol bytes to client) would persist; R7 free-request gap would persist.

**Choice: A.** Three-mode split. The streaming case keeps its current `stream_probe` (no change) and gains an SSE error frame on late `parse_response` failure (T02 sub-step B). The non-stream cross-protocol case gains the cap + parse-fail 502.

**Why A over B.** B is architecturally impossible without a Pingora fork (filter API contract). A is the minimum diff that fixes R24 + R7 without regressing streaming behaviour.

### ADR-W6-02 — REV-FINAL-H6.2 grace-expiry emit (Proxy-6)

**Context.** Cancel-grace `Expired` (gateway.rs:1483-1492) returns `Err(Custom("cancel_grace_expired"))` directly without calling `emit_response_surfaces` (gateway.rs:637). On the happy path `emit_response_surfaces` runs at `end_of_stream` (gateway.rs:1571-1578). For a cancelled-then-expired request, the archive XADD never fires; detailed-log shows a hole.

**Options.**
- **A. Emit before Err (v2 H6.2).** Call `emit_response_surfaces(ctx, /*force_flush_partial=*/true)` before `return Err(...)`. Internal failures inside the emit are swallowed (only metric + log) so the outer `Err` is preserved. Add a `bool` second argument named `force_flush_partial` so the function can compress partial state into the archive payload.
- **B. Emit in `logging` filter only.** Pingora calls `logging` after every filter returns. Move all archive emission to `logging`. Rejected: `logging` runs after Pingora has already finalized the request; the existing emission point in `response_body_filter` was chosen for correct upstream/downstream pairing. Splitting would require larger context survey.
- **C. Spawn an emit task.** `tokio::spawn(emit_response_surfaces(ctx_clone, true))` and return Err immediately. Rejected: ReqCtx is not `Clone`; spawning would race with `logging` filter mutating ctx.

**Choice: A.** Synchronous emit-then-Err in the Expired arm.

**Why A over B/C.** Surgical, no new control flow; the swallow-internal-failures rule keeps the outer Err observable.

### ADR-W6-03 — REV-FINAL-H6.3 reset matrix (R8)

**Context.** `reset_response_attempt_state` (gateway.rs:131-153) clears 13 of ~70 `ReqCtx` fields. See §0.3 for the full leak audit.

**Options.**
- **A. Field-by-field classification + CI lint.** Produce a `RESET_FIELDS.md` table listing every `ReqCtx` field with one of four classifications: `persist` (cross-attempt by design, e.g. `request_id`), `attempt-reset` (cleared in `reset_response_attempt_state`), `request-end-reset` (cleared at `logging` end), `protocol-only` (never crosses a filter boundary). Add `scripts/check-reqctx-reset.sh` (a `rust-analyzer`-driven syn-parser script) that fails CI if a new `ReqCtx` field is added without a classification entry.
- **B. Wrap ReqCtx in a `PerAttempt` substruct.** Move all attempt-scoped fields into `ReqCtx.per_attempt: PerAttempt`, and replace `reset_response_attempt_state` with `ctx.per_attempt = Default::default()`. Rejected as **first PR**: requires touching every filter that reads any attempt field (~30 sites); high regression risk; would gate the WS-6 PR on unrelated refactors. Recorded as a follow-up.
- **C. Selectively add fields to the existing function.** Just append the missing field clears to `reset_response_attempt_state`. Rejected as **only** mechanism: same drift recurs whenever someone adds a new field.

**Choice: A + selective fix.** Land the missing clears immediately (selective fix; merges fast) AND add the classification doc + CI lint so the next field added is classified by force.

**Why A over B.** B is the long-term right answer (see follow-up FILL-W6-01 in §5 below) but ships in a later PR; A unblocks WS-6 launch gate.

### ADR-W6-04 — REV-FINAL-H6.4 archive channel hardening (Proxy-9 partial / Proxy-10)

**Context.** `DetailedArchive` (gw-ledger/src/detailed_archive.rs:120-150) is **already** `tokio::sync::mpsc::Sender<CapturedSurface>` with bounded `channel_capacity` (default from `[detailed_log]` config) and uses `try_send` with drop+metric `gw_detailed_log_drop_total{reason="channel_full", kind=...}`. **The bounded mpsc + try_send + overflow metric is already shipped.** The v2 H6.4 candidate of "convert mpsc to bounded + try_send" is already done; the residual gaps are:

| Residual gap | Action |
|---|---|
| Metric name uses `kind` not `surface` per v2 nomenclature | Rename label `kind` → `surface` is breaking; **keep `kind` to avoid Grafana migration**. Document the divergence in the task. |
| Default `channel_capacity` and `workers` are wired from `[detailed_log]` in `config/default.toml`; v2 suggested cap 4096 | Audit the current default; if < 4096, raise to 4096; if > 4096 with operational evidence, document the choice. |
| No alert rule on `gw_detailed_log_drop_total{reason="channel_full"} > 0` for 5min | Add alert + runbook entry (coordinate with WS-10). |
| Per-surface fairness: a flood of one `kind` can starve others | Defer to FILL-W6-02 (priority partitioning); not in scope for WS-6. |

**Options.**
- **A. Accept current `kind` label, raise cap to 4096 if lower, add alert.** Minimal-diff.
- **B. Split per-surface channels (one mpsc per `Kind`).** Better fairness; larger diff; requires worker re-architecture. Defer.

**Choice: A.**

**Why A over B.** B is the right shape for fairness but is a 2-3 day refactor; not WS-6 launch-gate blocker.

### ADR-W6-05 — REV-FINAL-H6.5 spawn pools (Proxy-5 / Proxy-10)

**Context.** `billing_lifecycle.rs:431` and several sites in gateway.rs use bare `tokio::spawn` for billing emit, counter emit, quota updates, freeze release, lease release. No timeout, no concurrency cap, no per-category budget. Under a slow Redis or a Sentinel partition, every in-flight request adds 5+ spawned futures, none of which drain; Tokio scheduler queue grows, GC of completed JoinHandles cannot keep up.

**Options.**
- **A. Per-category spawn pools (v2 H6.5).** Three categories with distinct policies:
  - *billing emit* — MUST complete; no timeout; on failure, spill to disk backlog (shared with WS-1 R7 spill).
  - *counter emit* — bounded queue (`tokio::sync::Semaphore` with permits = configurable, default 256); drop + metric on permit-exhausted.
  - *freeze/lease release* — MUST complete; no timeout; idempotent by CAS so re-issuable; on failure, spill to disk and retry from sweeper.

  Implementation: a new struct `crate::spawn_pool::SpawnPools` with three methods `spawn_billing(fut)`, `spawn_counter(fut)`, `spawn_release(fut)`. Each holds its own `Arc<Semaphore>` or backlog handle. The bare `tokio::spawn` sites are replaced one-for-one.
- **B. Single global Semaphore (v1 S6.5).** Rejected per v2 explicit text: "**删全局 Semaphore**". A global semaphore would let a billing flood starve releases.
- **C. Do nothing, rely on Redis client backpressure.** Rejected: the spawned future itself is the queue; Redis backpressure is downstream.

**Choice: A.**

**Why A over B.** Per-category isolation prevents one slow path (counter emit) from delaying the must-complete path (billing emit, lease release). v2 §REV-FINAL-H6.5 explicitly rules out B.

### ADR-W6-06 — REV-FINAL-H6.6 block_in_place removal (Proxy-9)

**Context.** `response_body_filter` (gateway.rs:1453) wraps four `await`s in `tokio::task::block_in_place(|| rt.block_on(...))`:

| Line | Wrapped call | Reason in comment |
|---|---|---|
| **1495** | `filters::response::run(session, body, end_of_stream, ctx)` | filter is async; Pingora callback is sync |
| **1503** | `self.classify_final_upstream_error(ctx)` | reads archive via `read_for_emit` |
| **1519** | `ctx.archive_upstream_response_body.read_for_emit()` | re-read for error-passthrough HTML detection |
| **1574** | `self.emit_response_surfaces(ctx)` | XADD path + archive flush |

`block_in_place` parks the current Tokio worker and moves it to the blocking pool. With N workers (default `runtime_workers = num_cpus`), four consecutive `block_in_place` per request can starve the runtime under load.

**Options.**
- **A. Make `response_body_filter` itself async-friendly via Pingora's hook.** Pingora 0.8 `ProxyHttp::response_body_filter` is sync per the trait. To do async, the work must be hoisted to a Pingora hook that IS async. Pingora 0.8 exposes `logging` as async-trait. Move post-EOS work (emit, classify_final, error-passthrough re-render) into `logging` and out of `response_body_filter`. **The per-chunk `filters::response::run` MUST stay** in `response_body_filter` because it mutates the outbound `body` chunk before pingora forwards it.
- **B. Replace `block_in_place(rt.block_on(...))` with `futures::executor::block_on`.** Rejected: still blocking; runs on the Tokio worker rather than the blocking pool, strictly worse.
- **C. Pre-compute everything in `response_filter` and inline in `response_body_filter`.** Rejected: cannot move body inspection earlier (Pingora API).

**Choice: A.** Move `classify_final_upstream_error`, `error-passthrough` re-render, and `emit_response_surfaces` into the async `logging` filter. Keep only the per-chunk `filters::response::run` (line 1495) in `response_body_filter`. **One `block_in_place` remains** for that single per-chunk call — investigate whether `filters::response::run` can be made sync (its current async-ness comes from one `await` on archive append; that append can become `try_send` since we already buffer in `ArchiveState::InMemory` for small payloads). If `filters::response::run` can be made sync, then ZERO `block_in_place` remains.

**Why A over B/C.** A is the only path that actually unblocks Tokio workers. The remaining `block_in_place` (if any) is bounded to one short async call rather than four.

### ADR-W6-07 — REV-FINAL-H6.7 cross→same-protocol mid-stream switch test

**Context.** No existing e2e tests a request that begins cross-protocol (e.g. Anthropic client → OpenAI upstream) and mid-stream is force-switched to same-protocol (e.g. failover to Anthropic upstream). The risk is byte garbling: the codec re-render machinery prepared for cross-protocol output but mid-stream switches mean state is mid-conversion.

**Options.**
- **A. Add a `crates/gw-proxy/tests/e2e_cross_same_switch.rs` integration test** that runs against the fake upstream harness with a forced mid-stream failover at chunk N=3.
- **B. Add a unit test on `stream_state` only.** Rejected: doesn't catch the byte-fidelity issue at the client edge.

**Choice: A.**

---

## 3. Tasks (8-section format per v2 §0)

### TASK W6-T01 — Grace-expiry path flushes `emit_response_surfaces` before Err

**【是什么 What it IS】**
- A change to `crates/gw-proxy/src/gateway.rs`, function `response_body_filter` (lines 1453-1580), branch `crate::cancellation::GraceStatus::Expired` (lines 1483-1492).
- Insert one async call to `self.emit_response_surfaces(ctx)` (which is `gateway.rs:637`) before `return Err(Error::new(ErrorType::Custom("cancel_grace_expired")))` on line 1491.
- Modify `emit_response_surfaces` signature: add a second parameter `force_flush_partial: bool`. When `true`, emit even if `archive_upstream_response_meta` is `None` (use synthetic meta `status=ctx.upstream_status.unwrap_or(0)`, `headers=Vec::new()`). When `false`, behaviour is unchanged.
- Both `try_send` calls inside `emit_response_surfaces` must remain best-effort; their internal failures must NOT propagate up (covered by the existing `try_send` impl in `gw-ledger/src/detailed_archive.rs:140-150`).

**【不是什么 What it is NOT】**
- Not a change to `cancellation::check_grace_status` behaviour.
- Not a change to the metric `record_grace_outcome(yielded)` (line 1490 stays as-is).
- Not a change to the error type `Custom("cancel_grace_expired")`.
- Not a rewrite of `emit_response_surfaces` body — only signature + one new branch for `force_flush_partial`.

**【为什么 Why this bug exists / why fix】**
- Root cause: the Expired arm was written before the detailed-archive subsystem became mandatory; the early-return predates the requirement that every request emits a UpstreamResponse + DownstreamResponse surface.
- Audit: Proxy-6 (original Rust audit), v2 REV-FINAL-H6.2.
- Consequence today: every cancel-then-grace-expired request leaves a hole in `usage:detailed`. Postmortem on a customer complaint cannot replay the upstream body. SLO `detailed_log_emit_total / request_total ≥ 0.999` fails under cancellation storms.

**【做什么 DO】**
1. Edit `gateway.rs:637` — change `async fn emit_response_surfaces(&self, ctx: &mut ReqCtx)` to `async fn emit_response_surfaces(&self, ctx: &mut ReqCtx, force_flush_partial: bool)`.
2. Inside `emit_response_surfaces`, guard the `if let Some(meta) = ctx.archive_upstream_response_meta.take()` block: if `meta` is `None` and `force_flush_partial` is true, build a synthetic `ResponseMeta { status: ctx.upstream_status.unwrap_or(0), headers: Vec::new() }` and proceed; otherwise (the existing case) keep current behaviour.
3. Update the only existing call site (`gateway.rs:1576`): `rt.block_on(self.emit_response_surfaces(ctx, false))` — `false` preserves current happy-path semantics. Note: after W6-T07 lands, this call site moves into `logging` filter; coordinate ordering.
4. Inside the `GraceStatus::Expired` arm (gateway.rs:1483-1492), BEFORE line 1490 `crate::cancellation::record_grace_outcome(yielded);`, add a `block_in_place + block_on` call to `self.emit_response_surfaces(ctx, true)`. (After W6-T07 lands, this also moves to `logging`. For ordering: T01 ships before T07; T07 then refactors both call sites together.)
5. Add a metric: `metrics::counter!("gw_archive_emit_grace_flush_total").increment(1);` immediately after the new call so the operator sees grace-expired emit count.

**【不做什么 DON'T】**
- DON'T propagate any error from `emit_response_surfaces` out of the Expired arm — the customer-visible `Err` is the grace cancellation, not the archive emit.
- DON'T change the order: `emit_response_surfaces` MUST run before `record_grace_outcome(yielded)` so the metric reflects what was emitted.
- DON'T make `force_flush_partial=true` the new default; that would alter behaviour in non-grace paths.
- DON'T add a synthetic `archive_downstream_response_meta` in the same way — downstream meta is captured at EOS in `response_body_filter` (gateway.rs:1500+), which the Expired arm never reaches; a fully-synthetic downstream meta gives the wrong status. Use `ctx.upstream_status.unwrap_or(0)` for downstream too because for cancel-grace-expired the client likely never received a final status; this matches what an EOS-less ledger would see.
- DON'T add an `unwrap_or_else` that panics — any failure inside this path is best-effort.

**【怎么做 HOW (具体)】**

Patch (literal):

```rust
// gateway.rs, around line 637 — function signature change
-    async fn emit_response_surfaces(&self, ctx: &mut ReqCtx) {
+    async fn emit_response_surfaces(&self, ctx: &mut ReqCtx, force_flush_partial: bool) {
         use crate::ctx::{ArchiveState, ResponseMeta};
         use gw_ledger::detailed_archive::{CapturedSurface, Kind, ResponsePayload, SurfacePayload};
         let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;

-        // upstream_response — paired with the headers snapshot taken in response_filter
-        if let Some(meta) = ctx.archive_upstream_response_meta.take() {
+        // upstream_response — paired with the headers snapshot taken in response_filter
+        let upstream_meta = ctx.archive_upstream_response_meta.take().or_else(|| {
+            if force_flush_partial {
+                Some(ResponseMeta {
+                    status: ctx.upstream_status.unwrap_or(0),
+                    headers: Vec::new(),
+                })
+            } else {
+                None
+            }
+        });
+        if let Some(meta) = upstream_meta {
             let body_bytes = match ctx.archive_upstream_response_body.read_for_emit().await {
                 // ... unchanged body
```

Apply the symmetric change to the downstream block at gateway.rs:680-723.

```rust
// gateway.rs, around line 1483 — Expired arm
             crate::cancellation::GraceStatus::Expired => {
                 // Grace window elapsed. Heuristic: if VendorExposure reached
                 // Settled the extractor saw a terminal frame during grace.
                 let yielded = matches!(
                     ctx.vendor_exposure,
                     crate::billing_lifecycle::VendorExposure::Settled,
                 );
+                // REV-FINAL-H6.2: flush whatever archive surfaces we have
+                // BEFORE recording outcome + returning Err, so detailed-log
+                // is not silently empty for cancelled requests.
+                tokio::task::block_in_place(|| {
+                    let rt = tokio::runtime::Handle::current();
+                    rt.block_on(self.emit_response_surfaces(ctx, true))
+                });
+                metrics::counter!("gw_archive_emit_grace_flush_total").increment(1);
                 crate::cancellation::record_grace_outcome(yielded);
                 return Err(Error::new(ErrorType::Custom("cancel_grace_expired")));
             }
```

Update line 1576 call site:
```rust
-                rt.block_on(self.emit_response_surfaces(ctx))
+                rt.block_on(self.emit_response_surfaces(ctx, false))
```

**【如何验证 VERIFY】**

1. Unit (new file `crates/gw-proxy/tests/grace_expiry_emit.rs`):
   ```bash
   cargo test -p gw-proxy --test grace_expiry_emit -- --nocapture
   ```
   Test body: spin a `LlmGateway` with a mock `DetailedArchive` (use an in-memory `mpsc::Receiver<CapturedSurface>` for assertion). Craft a `ReqCtx` with `cancel_observed_at_ms = Some(now - grace_ms - 1)` and call `response_body_filter` directly. Assert: receiver gets 2 `CapturedSurface` (UpstreamResponse + DownstreamResponse) before the function returns Err; metric `gw_archive_emit_grace_flush_total` == 1.

2. Integration (extend `tests/e2e/client/soak/`): add scenario `chaos-grace-expiry`. Drive 50 cancelled-mid-stream requests; assert `Σ detailed_log_emit_total{kind=UpstreamResponse} == 50` (one per cancelled request).
   ```bash
   make e2e-soak-smoke -- --scenario chaos-grace-expiry
   ```

3. Regression: `cargo test --workspace` MUST pass; in particular `crates/gw-proxy/tests/cancellation_*` must not regress.

4. Output artifact: `logs/W6-T01-verify.txt` with both test outputs, must contain `test result: ok. .* 0 failed` and `gw_archive_emit_grace_flush_total 50`.

**【Dependencies】**
- Blocked by: none.
- Blocks: W6-T07 (which moves the same call into `logging` filter and removes both `block_in_place` wrappers).
- Same-PR sibling: none.

**【Adversarial review】**
After landing, dispatch a sonnet review with prompt template `tdd-adversarial-review.txt`. Focus checks:
- (a) Does `force_flush_partial=true` ever leak a `Some(meta)` value back into ctx for a later filter? (Answer should be no: we `.take()` it.)
- (b) Does the synthetic downstream meta (`status=upstream_status.unwrap_or(0)`) ever lie? (Document: yes, it can be 0 if upstream never replied; the archive consumer must tolerate status=0.)
- (c) Is the new `block_in_place` inside the Expired arm a new offender of REV-FINAL-H6.6? (Yes; W6-T07 removes it.)
- (d) Race: can `archive_upstream_response_meta` be `None` because `response_filter` short-circuited before line 1087? Check call paths.

---

### TASK W6-T02 — Three-mode preflight (REV-FINAL-H6.1, Proxy-7, R24, Proxy-8 hookup)

**【是什么 What it IS】**
- A change spanning `crates/gw-proxy/src/gateway.rs` (response_filter + response_body_filter), `crates/gw-proxy/src/filters/response.rs` (non-stream branch), and the codec error-frame emitter in `crates/gw-codec/src/error_passthrough.rs`.
- Three concrete sub-changes:
  - **Sub-A (Header-only mode)**: explicit branch in `response_filter` that handles `status >= 400` and egress-class headers via the existing error-classification path (already there; no change required, but add a docstring).
  - **Sub-B (Stream-probe mode)**: unchanged behaviour for first-N probe (gateway.rs:1370-1450 stays). NEW: on late stream `parse_response` failure (raised inside `filters::response::handle_stream_chunk`), instead of bubbling up via Err (which would race with already-committed body), inject an SSE `event: error\ndata: {"code":"GW_UPSTREAM_PROTOCOL_DECODE_ERROR","message":"upstream codec parse failed"}\n\n` frame into the outbound `body` and let the stream terminate naturally.
  - **Sub-C (Non-stream cross-protocol parse mode)**: in `filters::response::handle_non_stream_chunk`, before any byte goes downstream, check `ctx.upstream_body_accum.len() > [proxy.upstream_body_accum_cap_bytes]` — if so, set `ctx.upstream_error_code = Some(GW_UPSTREAM_BODY_TRUNCATED)`, drop the body, return `Err`. On EOS, if `parse_response` returns Err and `body_started == false`, set `upstream_error_code = Some(GW_UPSTREAM_PROTOCOL_DECODE_ERROR)`, return Err.

**【不是什么 What it is NOT】**
- NOT a universal preflight buffer. ADR-W6-01 rules out option B.
- NOT a change to Pingora's `ProxyHttp` filter ordering.
- NOT a change to `stream_probe.rs` first-N logic (that lives in WS-3's scope for cap; this task only touches the parse-fail late path).
- NOT a new HTTP error code: `GW_UPSTREAM_PROTOCOL_DECODE_ERROR` and `GW_UPSTREAM_BODY_TRUNCATED` already exist in `crates/gw-core/src/error/codes.rs` (verify by `rg 'GW_UPSTREAM_PROTOCOL_DECODE_ERROR' crates/gw-core/src/error/`; if missing, add them — both should be present since gateway.rs:240 references the former).
- NOT the lease/billing side of R7. WS-1 task T17 owns lease release + `credits_consumed = Some(0)`.

**【为什么 Why this bug exists / why fix】**
- Root cause Proxy-7: response_filter cannot inspect body; an `Err` at parse time arrives after the chunk is forwarded. Verified at gateway.rs:1453-1580 (`response_body_filter` returns `Ok` once `filters::response::run` mutates `body`; Pingora sends the chunk immediately after).
- Root cause R24: cross-protocol non-stream `parse_response` returns Err; current code in `filters/response.rs` (TODO: confirm line) ships `ctx.upstream_body_accum` verbatim to the client. The client receives Anthropic-shaped JSON when it asked for OpenAI Chat.
- Root cause R7: under `protected mode` (`ctx.lease_guard.is_some()`), the same parse failure additionally returns `Ok(0)` credits — gateway responds 200 with empty body, user pays nothing for an actual upstream call.
- Root cause Proxy-8: `upstream_body_accum` is an `Option<BytesMut>` with no cap — a megaresponse upstream can OOM the gateway.

**【做什么 DO】**
1. Add `proxy.upstream_body_accum_cap_bytes` to `crates/gw-proxy/src/config.rs` (or wherever ProxyCfg lives — search by `grep -rn 'pub struct ProxyCfg' crates/gw-proxy/src`); default `8 * 1024 * 1024`. Wire from `[proxy]` table in `config/default.toml`. Document in `docs/architecture.md` §Configuration.
2. In `filters/response.rs::handle_non_stream_chunk` (locate via `grep -n 'fn handle_non_stream_chunk\|upstream_body_accum' crates/gw-proxy/src/filters/response.rs`), add the cap check on each append:
   ```rust
   if let Some(ref mut accum) = ctx.upstream_body_accum {
       if accum.len() + chunk.len() > self.cfg.upstream_body_accum_cap_bytes {
           ctx.upstream_error_code = Some(gw_core::error::codes::GW_UPSTREAM_BODY_TRUNCATED);
           metrics::counter!("gw_proxy_upstream_body_accum_cap_hit_total").increment(1);
           return Err(pingora::Error::new(pingora::ErrorType::Custom("upstream_body_truncated")));
       }
       accum.extend_from_slice(&chunk);
   }
   ```
3. In the same function, at EOS branch, on parse failure when `!ctx.body_started`:
   ```rust
   ctx.upstream_error_code = Some(gw_core::error::codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR);
   metrics::counter!("gw_proxy_parse_response_failed_total",
       "client_kind" => client_kind.as_str().to_string(),
       "upstream_kind" => upstream_kind.as_str().to_string(),
   ).increment(1);
   // Drop the verbatim body — the W6-T08 fail_to_proxy path will render a JSON error.
   *body = None;
   return Err(pingora::Error::new(pingora::ErrorType::Custom("upstream_protocol_decode_error")));
   ```
4. For the streaming sub-B: locate `parse_stream_chunk` calls (`grep -n 'parse_stream_chunk' crates/gw-proxy/src/filters/response.rs`). On `Err`, when `ctx.client_body_committed == true` (so we cannot flip status), call new helper `crate::filters::response::emit_sse_error_frame(client_kind, GW_UPSTREAM_PROTOCOL_DECODE_ERROR, &mut body)` that appends the SSE error frame. The codec-side helper lives in `crates/gw-codec/src/error_passthrough.rs::render_sse_error_frame(client_kind, code, msg) -> Bytes`.
5. Add `render_sse_error_frame` to `crates/gw-codec/src/error_passthrough.rs`. Format per client kind:
   - OpenAI Chat: `data: {"error":{"code":"GW_UPSTREAM_PROTOCOL_DECODE_ERROR","message":"...","type":"upstream_decode_error"}}\n\ndata: [DONE]\n\n`
   - Anthropic Messages: `event: error\ndata: {"type":"error","error":{"type":"api_error","message":"..."}}\n\n` then `event: message_stop\ndata: {"type":"message_stop"}\n\n`
   - Gemini Generate: SSE-not-applicable for Gemini stream — Gemini uses chunked JSON arrays; for Gemini stream, emit `{"error":{"code":500,"message":"GW_UPSTREAM_PROTOCOL_DECODE_ERROR: ..."}}` as the final array element and close the array. Verify the on-wire Gemini stream framing by `grep -rn 'GeminiStreamWriter\|gemini.*stream' crates/gw-codec/src/`.

**【不做什么 DON'T】**
- DON'T add a universal preflight buffer for streaming. Streaming MUST commit early; the SSE error frame is the only honest signal.
- DON'T ship `upstream_body_accum` bytes to the client on parse failure, even partially — that's R24 verbatim.
- DON'T conflate the streaming late-failure with the non-stream early-failure: the former emits an SSE frame and lets the stream end; the latter returns Err and lets `fail_to_proxy` (or the error-passthrough machinery) render a JSON 502.
- DON'T forget to clear `ctx.upstream_body_accum` after dropping (W6-T03's reset matrix covers this on retry, but on the terminal-failure path the EOS path here MUST also `ctx.upstream_body_accum = None` to free memory).
- DON'T set `ctx.body_started = true` after the parse failure — that would defeat any later failover. (Check whether a non-stream parse failure should be retryable; the v1 design says retryable but `body_started` check would block. Document the choice.)
- DON'T introduce a new error code; reuse `GW_UPSTREAM_PROTOCOL_DECODE_ERROR` and `GW_UPSTREAM_BODY_TRUNCATED` which already exist (gateway.rs:210, 240 reference them).

**【怎么做 HOW (具体)】**

Step-by-step. Search first, then patch:

```bash
# 1. Confirm error codes exist
rg -n 'GW_UPSTREAM_PROTOCOL_DECODE_ERROR|GW_UPSTREAM_BODY_TRUNCATED' \
  crates/gw-core/src/error/codes.rs

# 2. Locate the non-stream handler
rg -n 'fn handle_non_stream_chunk|upstream_body_accum' \
  crates/gw-proxy/src/filters/response.rs

# 3. Locate ProxyCfg
rg -n 'pub struct ProxyCfg' crates/gw-proxy/src

# 4. Locate the stream parse_stream_chunk call
rg -n 'parse_stream_chunk' crates/gw-proxy/src/filters/response.rs

# 5. Locate the Gemini stream framing for the SSE-error helper
rg -n 'GeminiStreamWriter|gemini.*stream' crates/gw-codec/src/
```

Then apply the patches described in the DO block. For the new `render_sse_error_frame` helper, follow the pattern of `render_error_to_client` already at `gw-codec/src/error_passthrough.rs` (referenced from gateway.rs:1555).

**【如何验证 VERIFY】**

1. Unit (cap-hit, non-stream):
   ```bash
   cargo test -p gw-proxy --test non_stream_cap_hit -- --nocapture
   ```
   Drive a 16 MiB chunked upstream into the non-stream handler with cap = 8 MiB. Assert: `upstream_error_code == GW_UPSTREAM_BODY_TRUNCATED`, counter `gw_proxy_upstream_body_accum_cap_hit_total == 1`, body output is `None`, function returns `Err`.

2. Unit (parse-fail, non-stream, cross-protocol):
   ```bash
   cargo test -p gw-proxy --test non_stream_cross_protocol_parse_fail -- --nocapture
   ```
   Feed Anthropic-shaped JSON to a request whose `client_codec=OpenAI`, `upstream_codec=Anthropic`. Assert: `upstream_error_code == GW_UPSTREAM_PROTOCOL_DECODE_ERROR`, body is `None`, returns `Err`.

3. Unit (SSE error frame, streaming, late failure):
   ```bash
   cargo test -p gw-codec --test sse_error_frame_render -- --nocapture
   ```
   For each `ApiShape::{OpenAiChat, AnthropicMessages, GeminiGenerate}`, assert that `render_sse_error_frame` produces the literal byte strings documented in DO step 5. Use snapshot tests via `insta`.

4. e2e (R24 regression):
   ```bash
   make e2e -- --filter cross_protocol_parse_fail_502
   ```
   The fake upstream replies with malformed JSON; assert client receives HTTP 502 + JSON `{"error":{"code":"GW_UPSTREAM_PROTOCOL_DECODE_ERROR",...}}` in their client format (OpenAI / Anthropic / Gemini renders), NOT the raw upstream bytes.

5. e2e (R7 regression — paired with WS-1 T17):
   ```bash
   make e2e -- --filter protected_mode_parse_fail_no_free_request
   ```
   Same parse failure but with `lease_guard.is_some()`. Assert (this PR's check): client gets 502. (WS-1 T17 checks: lease refund executed; `usage:records` emitted with `credits_consumed=Some(0)` + `vendor_exposure_state=Terminated`.)

6. Soak:
   ```bash
   make e2e-soak-smoke
   ```
   Must remain green; no new memory leak or runtime panic.

7. Output artifact: `logs/W6-T02-verify.txt`.

**【Dependencies】**
- Blocked by: error codes `GW_UPSTREAM_PROTOCOL_DECODE_ERROR` and `GW_UPSTREAM_BODY_TRUNCATED` present in `crates/gw-core/src/error/codes.rs` (verify; if missing, add as step 0 of this task).
- Blocks: W6-T08 (fail_to_proxy rendering of the 502 JSON body).
- Same-PR sibling: WS-3 task for `upstream_body_accum` cap (REV-FINAL-V1 RejectRequest policy) — coordinate via the merged Track-C PR (REV-003c).
- Same-PR sibling: WS-1 T17 (lease release on parse fail).

**【Adversarial review】**
Dispatch sonnet with focus on:
- (a) Does the SSE error frame respect each client-shape's expected framing? Test against a real OpenAI / Anthropic / Gemini SDK consuming the stream — does the SDK surface the error or silently treat it as `[DONE]`?
- (b) The cap check `accum.len() + chunk.len() > cap` — is it inclusive or exclusive of the existing buffer? Off-by-one means the cap is effectively `cap + chunk.len()`.
- (c) Memory: setting `*body = None` after the parse failure — does Pingora interpret this as "no body" or as "skip chunk and wait for more"? Verify against Pingora 0.8 docs / source.
- (d) Is the failure path retryable when `body_started == false`? Document.

---

### TASK W6-T03 — ReqCtx field-classification matrix + complete `reset_response_attempt_state` + CI lint

**【是什么 What it IS】**
- A change to `crates/gw-proxy/src/ctx.rs` adding `#[doc = "RESET_CLASS: persist|attempt-reset|request-end-reset|protocol-only"]` annotations on every `ReqCtx` field.
- A change to `crates/gw-proxy/src/gateway.rs` `reset_response_attempt_state` (lines 131-153) adding clears for every field classified as `attempt-reset`.
- A new doc `docs/architecture.md` §"ReqCtx field-reset matrix" listing every field with its class + justification.
- A new CI lint `scripts/check-reqctx-reset.sh` (bash + `syn`/`rust-analyzer`) that parses `ctx.rs`, extracts every field, verifies each has a `RESET_CLASS:` annotation, and verifies every `attempt-reset` field is cleared in `reset_response_attempt_state`.

**【不是什么 What it is NOT】**
- NOT a refactor of `ReqCtx` into a `PerAttempt` substruct (ADR-W6-03 option B — deferred to FILL-W6-01).
- NOT a change to retry semantics (failover trigger logic stays).
- NOT a change to fields that are correctly classified as `persist` (e.g. `request_id`, `trace_id`, `regex_cache`, `backpressure_gate`, `failover_attempt`, `attempt_index`, `attempted_keys`, `retry_blacklist`).
- NOT a change to `request-end-reset` fields (cleared at `logging` filter — currently not enforced; this task only adds the classification, not new clear logic).

**【为什么 Why this bug exists / why fix】**
- Root cause R8: the reset function was added piecemeal as each new attempt-scoped field landed; nothing forced authors to update it. §0.3 of this plan enumerates the missing clears.
- Audit: R8 (new finding), v2 REV-FINAL-H6.3.
- Consequence: Frankenstein bodies, mis-attributed quota, double-counted usage, stale error codes on successful retries. Production impact: customers pay for wrong attempt's usage; postmortem evidence corrupted.

**【做什么 DO】**
1. Read ALL of `ctx.rs:291-576` and classify every field in the `ReqCtx` struct definition into one of:
   - `persist` — value is set once per request and carries across all attempts (e.g. `request_id`, `attempt_index`).
   - `attempt-reset` — value is set during one attempt and MUST NOT leak into the next attempt's processing.
   - `request-end-reset` — value is set per request but cleared by `logging` filter (typically RAII guards).
   - `protocol-only` — value is filter-local; never read by another filter; classification is "n/a, doesn't need cross-filter coordination".
2. Edit `ctx.rs` to annotate each field (use Rust attribute doc-comment):
   ```rust
   /// RESET_CLASS: attempt-reset
   /// Cleared in reset_response_attempt_state; bleeds Frankenstein body on retry if missed.
   pub upstream_body_accum: Option<bytes::BytesMut>,
   ```
3. Edit `gateway.rs:131-153` and ADD the missing clears identified in §0.3:
   ```rust
   ctx.upstream_body_accum = None;
   ctx.operation_id = None;
   ctx.quota_snapshot = None;
   ctx.stream_state = None;
   ctx.usage = crate::ctx::UsageAccumulator::default();
   ctx.usage_provenance = gw_ledger::usage::UsageProvenance::default();
   ctx.error_outcome = None;
   ctx.error_passthrough_body = None;
   ctx.error_passthrough_active = false;
   ctx.cancel_grace_active = false;
   ctx.egress_sent_at = None;
   ctx.credits_consumed = None;
   ctx.coef_table_version_at_settle = None;
   ctx.upstream_error_code = None;
   ctx.upstream_error_detail = None;
   ctx.archive_upstream_response_meta = None;
   ctx.archive_downstream_response_meta = None;
   // resp_content_encoding is recaptured by response_filter on each attempt;
   // documented as self-healing; no explicit clear required (verify by reading
   // gateway.rs:1121 — set unconditionally when !is_stream).
   ```
4. Write `docs/architecture.md` §"ReqCtx field-reset matrix" — one table row per field, columns `Field | Class | Why | Cleared in`.
5. Write `scripts/check-reqctx-reset.sh`:
   ```bash
   #!/usr/bin/env bash
   set -euo pipefail
   # Parse ctx.rs with syn; for every field, require a RESET_CLASS: doc tag.
   # For every field tagged attempt-reset, require a corresponding ctx.<field> = ... line in
   # reset_response_attempt_state in gateway.rs.
   # Implementation: small Rust program in scripts/check-reqctx-reset/ that uses syn 2.x.
   ```
   Hook into `make check` and `scripts/check-invariants.sh`.

**【不做什么 DON'T】**
- DON'T add a clear for `vendor_exposure` in the reset function — `vendor_exposure` is a state machine that the `billing_lifecycle` settle path advances on retry; a brute-force reset would mis-record the transition. Classify it as `request-end-reset` with the justification "advanced by billing_lifecycle::on_retry_sync; clearing here would erase the Settled marker the cancel-grace path consults".
- DON'T clear `freeze_guard` or `lease_guard` — they are RAII guards; dropping them on every attempt would refund credits prematurely. Classify as `request-end-reset` with "released by Drop at end of request; cross-attempt behaviour is intentional (one freeze per request)".
- DON'T clear `bytes_sent_to_client` (AtomicU64) — that monotonic counter aggregates across the whole request.
- DON'T touch `signature_*` fields — set in request_filter; unchanged across attempts.
- DON'T clear `picked_candidates` — used by the failover executor to find the next candidate.
- DON'T silently re-add the same `picked_vendor_key` in the reset (the existing line 133 sets it to the NEW key passed in; preserve that).

**【怎么做 HOW (具体)】**

For each field, the classification decision tree:

```
Is the field a final/settled-once value (request_id, trace_id, conversation_id, mgmt_*, signature_*)?
  → persist
Is the field a RAII guard (freeze_guard, lease_guard, stream_slot, inflight_guard)?
  → request-end-reset (already handled by Drop)
Is the field overwritten by a filter that runs on every attempt (resp_content_encoding by response_filter:1121)?
  → attempt-reset, but mark "self-healing" so reset function need not clear
Is the field accumulated across multiple chunks of one attempt (usage, usage_provenance, upstream_body_accum, sse_leftover, stream_state)?
  → attempt-reset
Is the field a per-chunk transient (body, pending_client_body)?
  → attempt-reset (cleared so next attempt starts fresh)
Otherwise:
  → audit each remaining field one-at-a-time; do not guess
```

Then for the CI lint, use this stub for `scripts/check-reqctx-reset/src/main.rs`:

```rust
use syn::{File, Item, ItemStruct, ItemFn, Fields};
fn main() {
    let ctx_src = std::fs::read_to_string("crates/gw-proxy/src/ctx.rs").unwrap();
    let gw_src  = std::fs::read_to_string("crates/gw-proxy/src/gateway.rs").unwrap();
    let ctx_ast: File = syn::parse_str(&ctx_src).unwrap();
    let gw_ast:  File = syn::parse_str(&gw_src).unwrap();
    // ... extract ReqCtx struct fields + their doc-attribute RESET_CLASS:
    // ... extract reset_response_attempt_state body, find every `ctx.<ident> = ...`
    // ... for each attempt-reset field whose name is NOT in the reset body, fail with
    //     "ReqCtx field `{name}` is classified attempt-reset but not cleared in
    //      reset_response_attempt_state. Add the clear or change the classification."
}
```

**【如何验证 VERIFY】**

1. Unit (`crates/gw-proxy/tests/reset_attempt_state.rs`):
   ```bash
   cargo test -p gw-proxy --test reset_attempt_state -- --nocapture
   ```
   For each of the 17 newly-cleared fields, set it to a non-default value, call `reset_response_attempt_state(&mut ctx, new_key)`, assert it equals default.

2. Property test (`crates/gw-proxy/tests/reset_attempt_state_property.rs`): use `proptest` to fill ReqCtx with arbitrary attempt-scoped data, call reset twice in a row, assert idempotency.

3. CI lint:
   ```bash
   bash scripts/check-reqctx-reset.sh
   ```
   Expected output: `OK: 70 fields classified; 17 attempt-reset fields all cleared.` Exit code 0.

4. Mutation test: temporarily remove ONE field-clear line from `reset_response_attempt_state` and rerun the lint. Expected: lint fails with the exact field name in the error message.

5. Output artifact: `logs/W6-T03-verify.txt` plus the new doc `docs/architecture.md` §"ReqCtx field-reset matrix".

**【Dependencies】**
- Blocked by: none.
- Blocks: W6-T07 (since T07 moves some emit logic; the reset matrix doc should be authoritative before any further field churn).
- Same-PR sibling: `scripts/check-reqctx-reset.sh` is added to `make check`; coordinate with WS-12.

**【Adversarial review】**
Dispatch sonnet, focus:
- (a) Are there fields the v2 author missed entirely? Search `grep -n 'pub ' crates/gw-proxy/src/ctx.rs | grep -v '//' | wc -l` and compare against the matrix row count.
- (b) Does `resp_content_encoding` really self-heal in all paths? Trace every call into `response_filter` and confirm line 1121 always executes when `!ctx.is_stream`.
- (c) Are there RAII guards that DROP on `=` assignment but whose drop logic refunds credits? If so, the reset function would refund mid-flight; document.
- (d) Does the lint catch a NEW field added without classification? Test by adding a fake field locally; lint must fail.

---

### TASK W6-T04 — Bounded archive channel: audit + alert + (no rewrite needed)

**【是什么 What it IS】**
- A configuration + observability change. NOT a code change to `DetailedArchive` itself.
- Audit `config/default.toml [detailed_log] channel_capacity` and `workers` defaults. If `channel_capacity < 4096`, raise to 4096 (or higher with operational justification documented in PR description). If `workers < num_cpus / 2`, raise to `num_cpus / 2` (rationale: one worker per two cores for archive XADD throughput).
- Add a Prometheus alert in `docs/spec/01-observability.md` (or the existing alert spec): `gw_detailed_log_drop_total{reason="channel_full"} > 0 for 5m` → page.
- Add runbook entry in `docs/issues/` (or RUNBOOK.md if exists): `archive-channel-full.md` — diagnostic checklist + escalation path.
- Document in the same task that the v2 metric-rename suggestion (`gw_archive_channel_overflow_total{surface}`) is **explicitly declined** because the existing `gw_detailed_log_drop_total{reason="channel_full", kind="..."}` is functionally equivalent and renaming would break Grafana dashboards. Record this divergence in the task's ADR block (above) and in `docs/architecture.md` §Deliberate behaviours.

**【不是什么 What it is NOT】**
- NOT a change to `gw-ledger/src/detailed_archive.rs` (already bounded mpsc + try_send + metric per source verification at line 120-150).
- NOT a per-surface channel split (FILL-W6-02 — fairness work, deferred).
- NOT a Pingora-level change.

**【为什么 Why this bug exists / why fix】**
- Root cause Proxy-9 / Proxy-10 partial: the bounded channel + try_send + drop metric has already been delivered as part of an earlier WS3-era hardening (verified by reading `gw-ledger/src/detailed_archive.rs:127, 142-149`). What remains is operational visibility — without an alert, a Redis stall that drops thousands of surfaces is silent until an SLO check.
- v2 REV-FINAL-H6.4: the textual recommendation matches what is already implemented except for capacity audit and alerting.

**【做什么 DO】**
1. Inspect `config/default.toml [detailed_log]`:
   ```bash
   rg -A 8 '^\[detailed_log\]' config/default.toml
   ```
   Record the current `channel_capacity` and `workers` values in the task description.
2. If `channel_capacity < 4096`, raise to 4096. If you raise above 4096, write the justification in the PR description.
3. Add alert spec entry:
   ```yaml
   - alert: GwArchiveChannelFull
     expr: sum(rate(gw_detailed_log_drop_total{reason="channel_full"}[5m])) > 0
     for: 5m
     labels: { severity: page }
     annotations:
       summary: "Detailed-archive mpsc channel is dropping surfaces"
       runbook: "docs/runbooks/archive-channel-full.md"
   ```
4. Add `docs/runbooks/archive-channel-full.md` with: (i) check `redis-cli XLEN usage:detailed`; (ii) check `redis-cli INFO clients`; (iii) check Redis OOM via `gw_health_redis_oom_*` gauges; (iv) escalation tree.
5. Edit `docs/architecture.md` §Deliberate behaviours to note: "metric name `gw_detailed_log_drop_total` is preserved for Grafana backward-compat; v2 plan suggested rename declined."

**【不做什么 DON'T】**
- DON'T rename the metric.
- DON'T change the worker reception pattern (the `Arc<Mutex<Receiver>>` is intentional for fan-in; per-worker hot loops are not in scope).
- DON'T add per-surface channels (FILL-W6-02 deferred).
- DON'T silently raise the cap above 4096 without justification — large caps mask backpressure.

**【怎么做 HOW (具体)】**

```bash
# 1. Audit current values
rg -A 8 '^\[detailed_log\]' config/default.toml

# 2. If channel_capacity is below 4096, edit:
#    config/default.toml — set channel_capacity = 4096 (or higher with docs)

# 3. Edit alert spec
#    Append the YAML above to whichever file lists Prometheus rules (likely
#    docs/observability.md or a deploy/ Prom rules file)

# 4. Create runbook
mkdir -p docs/runbooks
$EDITOR docs/runbooks/archive-channel-full.md
```

**【如何验证 VERIFY】**

1. Unit (existing): `cargo test -p gw-ledger --test detailed_archive -- --nocapture` MUST stay green. (Confirms no regression to the existing try_send semantics.)

2. Cap-hit synthetic: run `tests/e2e/client/soak/scenarios/redis-stall.yaml` (WS-13 chaos harness; if not yet built, defer assertion). Assert: after 30s of Redis stall, `gw_detailed_log_drop_total{reason="channel_full"} > 0` AND no gateway panic AND request RPS only mildly degraded (no full stop).

3. Alert dry-run: `promtool test rules tests/prom/archive-channel-full.test.yaml` produces the alert at the expected time.

4. Output artifact: `logs/W6-T04-verify.txt`.

**【Dependencies】**
- Blocked by: none.
- Blocks: W6-T05 (spawn pools use the same architectural pattern; align naming).
- Same-PR sibling: WS-10 alerting work.

**【Adversarial review】**
- (a) Is 4096 a sound default? Compute: at p99 emit cost ≈ 5 ms per surface, 4096 ≈ 20 s of backlog at 1 emit/2 ms. Document.
- (b) Does the runbook actually identify the root cause path? Walk through with an SRE.

---

### TASK W6-T05 — Per-category spawn pools (REV-FINAL-H6.5, Proxy-5 + Proxy-10)

**【是什么 What it IS】**
- A new module `crates/gw-proxy/src/spawn_pool.rs` providing three concrete spawn functions:
  - `spawn_billing<F>(fut: F)` — no timeout; on JoinError or future panic, spill the operation to disk (filename: `{ALM_SPILL_ROOT}/billing/{rid}-{op_id}.bin`) and emit metric `gw_spawn_billing_spill_total`.
  - `spawn_counter<F>(fut: F)` — try-acquire a permit from an `Arc<Semaphore>` (default 256); if no permit, drop the operation and increment `gw_spawn_counter_dropped_total`.
  - `spawn_release<F>(fut: F)` — no timeout; idempotent ops (lease release, freeze release); on failure, spill to `{ALM_SPILL_ROOT}/release/{rid}-{op_id}.bin` for sweeper-retry.
- Each function returns nothing (or `Result<(), SpawnPoolFull>` for counter).
- The struct `SpawnPools` is constructed once in bootstrap and held in `LlmGateway` as `pub(crate) spawn_pools: Arc<SpawnPools>`.
- Replace every bare `tokio::spawn` in `crates/gw-proxy/src/` with the appropriate category call. Inventory:
  - `crates/gw-proxy/src/billing_lifecycle.rs:431` → `spawn_billing` or `spawn_release` (depending on the operation; see HOW).
  - `crates/gw-proxy/src/gateway.rs:1649` (on_retry_sync inflight emit) → `spawn_billing`.
  - Audit ALL `tokio::spawn` in `crates/gw-proxy/src/` and `crates/gw-runtime/src/` for replacement candidates.
- A WS-12 CI lint `scripts/check-tokio-spawn.rs` (Rust syn analyzer) forbids `tokio::spawn` outside `gw-runtime` and `gw-proxy/src/spawn_pool.rs`.

**【不是什么 What it is NOT】**
- NOT a global Semaphore (v2 explicitly forbids).
- NOT a thread pool — these still use the existing Tokio runtime.
- NOT a change to the actual billing/release SQL or Redis payloads.
- NOT a sweeper rewrite — `spawn_release`'s spill format must be readable by the existing freeze/lease sweeper (WS-1 T18); coordinate.

**【为什么 Why this bug exists / why fix】**
- Root cause Proxy-5: `tokio::spawn(emit_*)` has no timeout; under slow Redis, JoinHandles pile up.
- Root cause Proxy-10: same pattern for counter / quota emits.
- Audit: Proxy-5, Proxy-10. v2 REV-FINAL-H6.5.
- Consequence: a Redis slowdown turns into RAM exhaustion + Tokio scheduler thrash within ~60 s.

**【做什么 DO】**
1. Create `crates/gw-proxy/src/spawn_pool.rs`:
   ```rust
   use std::sync::Arc;
   use tokio::sync::Semaphore;

   pub struct SpawnPools {
       counter_sem: Arc<Semaphore>,
       spill_root: Arc<gw_runtime::SpillRoot>,
   }

   impl SpawnPools {
       pub fn new(counter_permits: usize, spill_root: Arc<gw_runtime::SpillRoot>) -> Arc<Self> {
           Arc::new(Self { counter_sem: Arc::new(Semaphore::new(counter_permits)), spill_root })
       }

       /// Must-complete; no timeout. On panic the JoinHandle yields JoinError;
       /// catch via catch_unwind in the future itself for billing operations
       /// that have spill-able payloads.
       pub fn spawn_billing<F>(&self, fut: F)
       where F: std::future::Future<Output = ()> + Send + 'static {
           tokio::spawn(fut);
           // No try_send tracking: billing operations themselves are responsible
           // for spilling on internal Err. See gw-ledger/src/flusher.rs spill.
       }

       /// Drop-on-overflow. Returns Err(SpawnPoolFull) if no permit.
       pub fn spawn_counter<F>(&self, fut: F) -> Result<(), SpawnPoolFull>
       where F: std::future::Future<Output = ()> + Send + 'static {
           match self.counter_sem.clone().try_acquire_owned() {
               Ok(permit) => {
                   tokio::spawn(async move {
                       let _g = permit;
                       fut.await;
                   });
                   Ok(())
               }
               Err(_) => {
                   metrics::counter!("gw_spawn_counter_dropped_total").increment(1);
                   Err(SpawnPoolFull)
               }
           }
       }

       /// Must-complete; idempotent. Use for freeze/lease release.
       pub fn spawn_release<F>(&self, fut: F)
       where F: std::future::Future<Output = ()> + Send + 'static {
           tokio::spawn(fut);
       }
   }

   #[derive(Debug)]
   pub struct SpawnPoolFull;
   ```
2. Bootstrap (gateway.rs `LlmGateway::new` around line 478): construct `SpawnPools` from `[proxy.spawn_pools]` config; store as `pub(crate) spawn_pools: Arc<SpawnPools>`.
3. Add config block in `config/default.toml`:
   ```toml
   [proxy.spawn_pools]
   counter_permits = 256
   ```
4. Audit every `tokio::spawn` in `crates/gw-proxy/src/`:
   ```bash
   rg -n 'tokio::spawn' crates/gw-proxy/src/
   ```
   For each hit, decide category:
   - billing_lifecycle inflight emit / `on_retry_sync` inflight register → `spawn_billing` (must-complete; the future itself spills via `gw-ledger/src/flusher.rs` on Err).
   - counter / quota / metric XADD → `spawn_counter` (drop-OK).
   - freeze/lease release → `spawn_release`.
5. Replace each `tokio::spawn(async move { ... })` with `self.spawn_pools.spawn_billing(async move { ... })` (or counter/release).
6. Add WS-12 CI lint `scripts/check-tokio-spawn.rs` that fails on any `tokio::spawn` outside `gw-runtime` and `spawn_pool.rs`.

**【不做什么 DON'T】**
- DON'T add a global `Semaphore` across all three categories. (v2: "**删全局 Semaphore**".)
- DON'T add a timeout to billing or release spawns — a timed-out billing emit silently loses the record; spill-on-Err is the correct backpressure.
- DON'T drop a `spawn_release` operation on permit exhaustion — release MUST happen (or the sweeper picks up the spill); no cap.
- DON'T fold the spill path into spawn_pool — the spilling happens inside the future's own logic; spawn_pool just hosts the future.
- DON'T introduce `tokio::time::timeout` on billing operations — they have their own redis-level deadline.

**【怎么做 HOW (具体)】**

For each `tokio::spawn` site, decide by reading what the future does:

| File:line | Future content | Category | Justification |
|---|---|---|---|
| `gw-proxy/src/billing_lifecycle.rs:431` | `inflight.register` + emit `UsageRecord` to `usage:records` | `spawn_billing` | Must-complete; spillable on Err via existing flusher. |
| `gw-proxy/src/gateway.rs:1649` (post-on_retry_sync) | inflight register for the retry attempt | `spawn_billing` | Same as above. |
| (any counter XADD) | counter emit / quota update | `spawn_counter` | OK to drop; observability-only. |
| (any freeze release) | `RequestFreeze::release` | `spawn_release` | Idempotent; refunds are the cost of dropping. |
| (any lease release) | `LeaseGuard::release_async` | `spawn_release` | Same. |

Audit + replace:
```bash
# Find sites
rg -n 'tokio::spawn\|tokio::task::spawn' crates/gw-proxy/src/

# For each, edit and replace with spawn_pools.<category>
# Re-run rg to confirm zero hits outside spawn_pool.rs and gw-runtime
```

**【如何验证 VERIFY】**

1. Unit (`crates/gw-proxy/tests/spawn_pool.rs`):
   - `spawn_counter` drops when permits exhausted; metric `gw_spawn_counter_dropped_total > 0`.
   - `spawn_billing` survives a slow future (sleep 5 s) without dropping.
   - `spawn_release` runs even when counter is exhausted.

2. Chaos (WS-13): `redis-stall-60s.yaml` — assert:
   - `gw_spawn_counter_dropped_total > 0` (counter emits dropped, as designed)
   - billing flush spill files appear in `${ALM_SPILL_ROOT}/billing/`
   - release flush spill files appear in `${ALM_SPILL_ROOT}/release/`
   - process RSS does NOT grow > 200 MB above baseline
   - request RPS recovers within 10s of Redis restoration

3. CI lint: `cargo run --bin check-tokio-spawn -- crates/gw-proxy/src/ crates/gw-runtime/src/` → exit 0; introduce a synthetic `tokio::spawn` in `gw-proxy/src/foo.rs` → exit non-zero.

4. Output artifact: `logs/W6-T05-verify.txt`.

**【Dependencies】**
- Blocked by: WS-1 T18 (freeze sweeper expects spill format); coordinate file naming.
- Blocks: W6-T06 (some block_in_place removal moves emits into spawn_release).
- Same-PR sibling: WS-12 CI lint.

**【Adversarial review】**
- (a) Does `counter_permits = 256` make sense? Compute: at p99 emit 5 ms, 256 ≈ 51k ops/sec headroom. Document.
- (b) Could `spawn_release` itself panic and silently leak the release? The future MUST be wrapped in `AssertUnwindSafe(catch_unwind(...))` inside each release impl; verify.
- (c) The lint must allow `gw-runtime`'s legitimate `tokio::spawn` (supervisor). Check the allow-list.
- (d) Cross-WS: ensure WS-2 supervised_spawn audit (REV-FINAL-W11) doesn't introduce a fourth category we missed.

---

### TASK W6-T06 — Reserved for future (deleted)

> Intentionally vacant. Renumbering follows ADR-W6-04 reorganisation; do not introduce a T06 task.

---

### TASK W6-T07 — Eliminate 4× `block_in_place` in `response_body_filter` (REV-FINAL-H6.6, Proxy-9)

**【是什么 What it IS】**
- A refactor that moves three of the four `block_in_place` sites in `response_body_filter` (gateway.rs:1503, 1519, 1574) into the async `logging` filter. The fourth (line 1495 — the per-chunk `filters::response::run`) is the harder case; this task evaluates two sub-options and lands the chosen one in the same PR.
- Pingora 0.8 `ProxyHttp::logging` is `async`; moving these post-EOS calls there lets them `await` naturally with zero `block_in_place`.
- Sub-option A (preferred for T07): make `filters::response::run` itself synchronous by ensuring its only `await` (the archive append) becomes try-send via the existing `ArchiveState::append_chunk` — note `append_chunk` is currently `async` because of the spill-to-disk path. The async-ness is real; we cannot trivially synchronise it. So:
- Sub-option B: keep ONE `block_in_place` for the per-chunk call (line 1495 surrogate) and document why; remove the other three (1503, 1519, 1574).
- This task lands sub-option B. A follow-up (FILL-W6-03) explores making `ArchiveState::append_chunk` sync via a write-ahead in-memory buffer + background flusher — out of WS-6 scope.

**【不是什么 What it is NOT】**
- NOT a change to Pingora's filter trait signatures.
- NOT a removal of `tokio::task::block_in_place` from the codebase (other crates may legitimately use it).
- NOT a rewrite of `emit_response_surfaces` — only relocation.

**【为什么 Why this bug exists / why fix】**
- Root cause Proxy-9: four `block_in_place` per request × N RPS = up to 4N worker-parks. Under load, Tokio scheduler thrashes.
- Audit: Proxy-9. v2 REV-FINAL-H6.6.
- Source-verified location (this plan author, 2026-06-17): gateway.rs:1495, 1503, 1519, 1574.

**【做什么 DO】**
1. Locate or add an async `logging` filter impl on `LlmGateway`:
   ```bash
   rg -n 'async fn logging' crates/gw-proxy/src/gateway.rs
   ```
   If present, extend; if absent, add the trait impl.
2. Move the following from `response_body_filter` into `logging`:
   - `classify_final_upstream_error(ctx).await` (currently gateway.rs:1503-1506) — runs at EOS only.
   - The error-passthrough re-render block (gateway.rs:1509-1564) — runs at EOS only and depends on `ctx.error_passthrough_active`.
   - `emit_response_surfaces(ctx, false).await` (gateway.rs:1571-1578) — runs at EOS only.
3. Restructure: `response_body_filter` becomes:
   ```rust
   fn response_body_filter(&self, session, body, end_of_stream, ctx) -> Result<...> {
       // Cancel/grace block (lines 1471-1493) stays; grace flush still uses
       // block_in_place because the early Err prevents reaching logging.
       // (T01's grace-flush block_in_place is the residual one. T07
       //  documents this as accepted; the SOLE remaining offender per ADR.)

       let res = tokio::task::block_in_place(|| {
           let rt = tokio::runtime::Handle::current();
           rt.block_on(filters::response::run(session, body, end_of_stream, ctx))
       });

       if res.is_ok() && body.as_ref().map(|b| !b.is_empty()).unwrap_or(false) {
           crate::stream_probe::mark_client_committed(ctx);
       }

       res
   }

   async fn logging(&self, _session: &mut Session, _e: Option<&pingora::Error>, ctx: &mut Self::CTX) {
       // Only run end-of-stream actions here. Pingora calls logging() ONCE
       // at the very end of the request lifecycle.
       self.classify_final_upstream_error(ctx).await;
       if ctx.error_passthrough_active {
           // ... the error-passthrough re-render that used to live in
           // response_body_filter; rewritten async.
       }
       self.emit_response_surfaces(ctx, false).await;
   }
   ```
4. **Critical subtlety**: the error-passthrough re-render in old code REPLACED `*body` (gateway.rs:1561 `*body = Some(rendered);`). That mutation only takes effect if it happens inside `response_body_filter` — `logging` does not get a `body` ref. Solution: pre-stage the rendered bytes into a `ctx.pending_passthrough_body: Option<Bytes>` inside `response_body_filter` (small sync work — JSON serialize the already-buffered archive bytes), and assign `*body = Some(...)` from `response_body_filter` AFTER the chunk arrives at EOS. Alternative: render in `response_body_filter` synchronously since the archive bytes are already in memory by EOS — the `read_for_emit().await` is only async because of the spill case. Audit: in practice, by EOS the archive accumulator is either `InMemory(BytesMut)` (sync) or `Spilled` (async file read). For the Spilled case at EOS the bytes likely cross 4 MiB; this is rare for error responses (small JSON). Justify: keep the EOS path async-aware via a `block_in_place` only if `ArchiveState::Spilled`; sync read otherwise.
5. Implement the conditional:
   ```rust
   let accumulated = match &ctx.archive_upstream_response_body {
       crate::ctx::ArchiveState::InMemory(b) => b.to_vec(),
       _ => {
           tokio::task::block_in_place(|| {
               let rt = tokio::runtime::Handle::current();
               rt.block_on(async {
                   ctx.archive_upstream_response_body.read_for_emit().await.unwrap_or_default()
               })
           })
       }
   };
   ```
   This degrades to `block_in_place` only when spilled (rare for error responses).

**【不做什么 DON'T】**
- DON'T attempt to make `ArchiveState::append_chunk` sync in this task — that's FILL-W6-03 follow-up.
- DON'T move `mark_client_committed` to `logging` — it must run per-chunk.
- DON'T leave a `block_in_place` on the EOS path for the non-spilled archive case — read in-memory sync.
- DON'T introduce a new tokio runtime (`Runtime::new()`) anywhere — always use the existing handle.

**【怎么做 HOW (具体)】**

Audit + apply:

```bash
# Confirm logging filter signature
rg -n 'fn logging' crates/gw-proxy/src/gateway.rs

# Apply the edit per DO step 3-5.

# After landing, audit residual block_in_place
rg -n 'block_in_place' crates/gw-proxy/src/gateway.rs
# Expected: 2 sites remain — the per-chunk filters::response::run at the top
# of response_body_filter, AND the conditional Spilled-only read above. Both
# documented.
```

**【如何验证 VERIFY】**

1. Unit (`crates/gw-proxy/tests/logging_emit.rs`):
   - Drive a request to EOS; assert `emit_response_surfaces` ran by inspecting the mock `DetailedArchive` receiver.
   - Drive a request that aborts mid-stream (no EOS); assert `emit_response_surfaces` did NOT run (logging may still be called by Pingora; the EOS guard inside logging prevents emit).

2. Tokio runtime metrics soak: `make e2e-soak-smoke` with `tokio_unstable` flag and runtime-metrics enabled. Assert `tokio_runtime_workers_busy_ratio_p99 < 0.85` (INVESTIGATE-007 gate).

3. Regression: `cargo test --workspace` MUST pass; in particular every test in `crates/gw-proxy/tests/` that exercises EOS behaviour.

4. `rg -c 'block_in_place' crates/gw-proxy/src/gateway.rs` → exactly 2 (the per-chunk and the Spilled-only read). Document.

5. Output artifact: `logs/W6-T07-verify.txt`.

**【Dependencies】**
- Blocked by: W6-T01 (grace flush) — T07 may also relocate the grace flush call.
- Blocked by: W6-T03 (reset matrix) — moving emit logic must not stale `archive_*_response_meta` clears.
- Blocks: none.
- INVESTIGATE-007 is closed by this task's verify step 2.

**【Adversarial review】**
- (a) Does Pingora ALWAYS call `logging` even when `response_body_filter` returned Err? Read pingora 0.8 source / docs. If logging is skipped on certain Err paths, the emit hole reopens.
- (b) Does `ctx` arrive in `logging` with the same instance Pingora passed to `response_body_filter`? Confirm via `Session.ctx` semantics.
- (c) The Spilled-case `block_in_place` is conditional but still present — does it ever fire in practice under prod load? Add a metric `gw_archive_logging_spill_block_total` and watch.

---

### TASK W6-T08 — R7 + R24 502 `GW_UPSTREAM_PROTOCOL_DECODE_ERROR` JSON body

**【是什么 What it IS】**
- Wires the `ctx.upstream_error_code = Some(GW_UPSTREAM_PROTOCOL_DECODE_ERROR)` set by W6-T02 into an actual HTTP 502 response with a JSON body in the client's protocol shape.
- Affects `crates/gw-proxy/src/gateway.rs` `fail_to_proxy` (search via `rg -n 'fn fail_to_proxy' crates/gw-proxy/src/gateway.rs`) AND coordinates with WS-1 T17's lease-side fix.
- Reuses `gw_codec::error_passthrough::render_error_to_client` (already imported in gateway.rs:1555).

**【不是什么 What it is NOT】**
- NOT the lease/billing side (WS-1 T17).
- NOT a new error code definition.
- NOT a change to FailoverExecutor.

**【为什么 Why this bug exists / why fix】**
- Root cause R7: parse failure currently returns 200 OK with empty body + `actual_usage_credits = Ok(0)`. Customer pays nothing for a real upstream call.
- Root cause R24: cross-protocol parse failure currently ships verbatim upstream bytes; customer SDK fails to parse.
- v2 REV-FINAL-H6.1 last bullet: "failure use SSE error event rather than flipping status" — that's the STREAMING case (W6-T02 sub-B). For non-stream, status FLIPPING is correct because the original 200 was never committed (we held the body in `upstream_body_accum`).

**【做什么 DO】**
1. Inside `fail_to_proxy`, on `ctx.upstream_error_code == Some(GW_UPSTREAM_PROTOCOL_DECODE_ERROR)` OR `Some(GW_UPSTREAM_BODY_TRUNCATED)`:
   - Force HTTP status 502.
   - Render JSON via `render_error_to_client(client_kind, &passthrough_struct_with_error_class)`.
   - Set response header `Content-Type` per client kind.
2. Add metric `gw_response_502_protocol_decode_total{client_kind, upstream_kind}`.
3. Coordinate with WS-1 T17: the same Err path triggers lease release; T17 owns the `LeaseGuard::release()` call + `UsageRecord { credits_consumed: Some(0), vendor_exposure_state: Terminated, parse_failed: true }` emission.

**【不做什么 DON'T】**
- DON'T set HTTP 500 (it's an upstream failure, hence 502).
- DON'T leak vendor identity in the JSON body (the upstream codec name is internal).
- DON'T re-render if `body_started == true` — that's the stream case; let SSE error frame handle it.

**【怎么做 HOW (具体)】**

```rust
// gateway.rs fail_to_proxy — add at top of function
if let Some(code) = ctx.upstream_error_code {
    if matches!(code, codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR | codes::GW_UPSTREAM_BODY_TRUNCATED)
       && !ctx.body_started {
        let client_kind = ctx.client_codec.as_ref()
            .map_or(gw_core::protocol::ProtocolKind::OpenAi, |c| c.kind());
        let passthrough = gw_core::error::PassthroughBody {
            upstream_status: 502,
            body_json: serde_json::json!({
                "code": code,
                "message": format!("upstream response could not be decoded ({})", code),
            }),
            is_sse: false,
        };
        let rendered = gw_codec::error_passthrough::render_error_to_client(
            client_kind, &passthrough,
            gw_core::error::ErrorClass::UpstreamProtocol,
            &self.error_passthrough_rules,
        );
        // ... set 502 status + Content-Type + body via session
        metrics::counter!("gw_response_502_protocol_decode_total",
            "client_kind" => client_kind.as_str().to_string()).increment(1);
        return; // skip default error handling
    }
}
```

**【如何验证 VERIFY】**

1. e2e (the same scenarios as W6-T02 verify steps 4 + 5).
2. Property test: for each `(client_kind, upstream_kind)` pair where they differ AND the upstream returns malformed JSON, assert client receives 502 + JSON in client's shape, not upstream's bytes.
3. Output artifact: `logs/W6-T08-verify.txt`.

**【Dependencies】**
- Blocked by: W6-T02.
- Same-PR with: WS-1 T17 (per v1 §6.1 line 1017).

**【Adversarial review】**
- (a) Does Pingora let `fail_to_proxy` set status + body cleanly? Verify against Pingora 0.8 API.
- (b) Could `body_started == true` legitimately occur for non-stream parse failure (e.g. response headers committed via something else)? Audit.
- (c) Does the WS-1 T17 lease-side fix run reliably from this same Err return? Coordinate.

---

### TASK W6-T09 — Cross→same-protocol mid-stream switch byte-fidelity test (REV-FINAL-H6.7)

**【是什么 What it IS】**
- A new integration test `crates/gw-proxy/tests/e2e_cross_same_switch.rs` (or under `tests/e2e/`).
- The test starts a request with `client_codec = Anthropic`, `upstream_codec = OpenAI` (cross-protocol re-render active), then mid-stream forces a failover to `upstream_codec = Anthropic` (same-protocol passthrough).
- Verifies that the client receives a byte-perfect, decoder-valid Anthropic SSE event sequence with no truncation, no leftover OpenAI bytes, and correct `message_start → content_block_*  → message_stop` framing.

**【不是什么 What it is NOT】**
- NOT a unit test of `stream_state` only.
- NOT a chaos scenario (though it could be added to WS-13 later).

**【为什么 Why this bug exists / why fix】**
- v2 REV-FINAL-H6.7: no such test exists; the risk is that mid-stream switch leaves codec accumulator state misaligned and produces garbled output.
- Reset matrix (W6-T03) should clear `stream_state` on reset; this test is the e2e proof.

**【做什么 DO】**
1. Use the fake-upstream harness (`crates/e2e-fake-upstream/`).
2. Configure two fake-upstream lanes: lane-A returns OpenAI stream, lane-B returns Anthropic stream. Lane-A injects a 5xx after the third chunk; failover policy retries on lane-B.
3. Drive the request from an Anthropic-shape client. Capture the full client byte stream.
4. Parse the captured bytes through `gw_codec::anthropic_messages::AnthropicSseReader` and assert: events in the legal order; no parse errors; `message_stop` arrives.

**【不做什么 DON'T】**
- DON'T assert exact bytes — vendor-specific delta JSON ordering may vary. Assert framing legality + key fields.
- DON'T silence intermittent failures with `#[ignore]`.

**【怎么做 HOW (具体)】**

Read existing similar tests:
```bash
ls crates/e2e-fake-upstream/tests/
rg -n 'cross_protocol' crates/gw-proxy/tests/ crates/e2e-fake-upstream/tests/
```

Pattern after `crates/e2e-fake-upstream/tests/inject.rs` (existing fault-injection pattern).

**【如何验证 VERIFY】**

```bash
cargo test -p gw-proxy --test e2e_cross_same_switch -- --nocapture
```
Assert: test passes; captured bytes parse cleanly through `AnthropicSseReader`; no panic.

Output artifact: `logs/W6-T09-verify.txt`.

**【Dependencies】**
- Blocked by: W6-T03 (reset matrix — without it, the test would fail for the wrong reason: leaked `stream_state`).
- Blocks: none.

**【Adversarial review】**
- (a) Does the fake-upstream harness support mid-stream upstream switch already, or does this require harness extension? If extension, file a WS-13 task.
- (b) Add the inverse direction (same→cross mid-stream switch) — does that path also need a test? Track as FILL-W6-04.

---

## 4. Task → finding-ID coverage matrix (sanity)

| Finding | Task(s) |
|---|---|
| Proxy-5 | W6-T05 |
| Proxy-6 | W6-T01 |
| Proxy-7 | W6-T02 |
| Proxy-8 | W6-T02 sub-C (cap) — also WS-3 owns the cap-trait introduction; W6-T02 is the hookup |
| Proxy-9 | W6-T07 (block_in_place); W6-T04 (channel bounded — already shipped) |
| Proxy-10 | W6-T05 (spawn pools); W6-T04 (archive channel) |
| R7 | WS-1 T17 (lease) + W6-T02 + W6-T08 (HTTP) |
| R8 | W6-T03 |
| R24 | W6-T02 + W6-T08 |
| REV-FINAL-H6.1 | W6-T02 |
| REV-FINAL-H6.2 | W6-T01 |
| REV-FINAL-H6.3 | W6-T03 |
| REV-FINAL-H6.4 | W6-T04 |
| REV-FINAL-H6.5 | W6-T05 |
| REV-FINAL-H6.6 | W6-T07 |
| REV-FINAL-H6.7 | W6-T09 |

Zero uncovered findings.

---

## 5. Follow-ups (not in this PR; tracked for later)

| ID | Description | Owner |
|---|---|---|
| **FILL-W6-01** | Refactor `ReqCtx` into `ReqCtx { core, per_attempt: PerAttempt }` so a new attempt is `core_ + Default::default()` (ADR-W6-03 option B). 2-3 day refactor. | Track C |
| **FILL-W6-02** | Per-surface DetailedArchive channels for fairness (split mpsc per `Kind`). | Track C |
| **FILL-W6-03** | Make `ArchiveState::append_chunk` non-async via in-memory write-ahead + background flusher; remove the last `block_in_place` in `response_body_filter`. | Track C |
| **FILL-W6-04** | Add same→cross protocol mid-stream switch test (inverse of W6-T09). | QA |

---

## 6. PR / Merge coordination (REV-003c restatement)

- Single PR — Track C owner.
- File touch surface: `gateway.rs`, `ctx.rs`, `filters/response.rs`, `spawn_pool.rs` (new), `gw-codec/src/error_passthrough.rs`, `gw-core/src/error/codes.rs` (verify), `gw-ledger/src/detailed_archive.rs` (no code change; alerts only), `config/default.toml`, `docs/architecture.md`, `docs/runbooks/archive-channel-full.md`, `scripts/check-reqctx-reset.sh`, `scripts/check-tokio-spawn.rs`.
- Out-of-PR coordination:
  - WS-1 T17 lands in WS-1's Rust PR; both PRs reference `GW_UPSTREAM_PROTOCOL_DECODE_ERROR`.
  - WS-3 cap-trait introduction lands in the merged Track-C PR before W6-T02 sub-C.
  - WS-12 hooks `scripts/check-reqctx-reset.sh` and `scripts/check-tokio-spawn.rs` into `make check`.
- Adversarial review day (per REV-007f): after PR is open, dispatch a sonnet review with prompt template `tdd-adversarial-review.txt` and focus instructions per each task's Adversarial review block.

---

## 7. Verification rollup (DoD restated with explicit thresholds, REV-005)

| Surface | Threshold | Tool |
|---|---|---|
| Parse-fail non-stream → 502 + JSON in client shape | 100% of `e2e --filter cross_protocol_parse_fail_502` runs | `make e2e` |
| Late streaming parse fail → SSE error frame in client codec | OpenAI / Anthropic / Gemini snapshot tests all pass | `cargo test -p gw-codec` |
| Grace-expired emit | 50/50 cancelled requests yield UpstreamResponse + DownstreamResponse XADD | `make e2e-soak-smoke -- --scenario chaos-grace-expiry` |
| ReqCtx reset matrix completeness | `scripts/check-reqctx-reset.sh` exits 0; new field added without classification fails | `make check` |
| `tokio::spawn` outside `gw-runtime` / `spawn_pool.rs` | zero hits | `cargo run --bin check-tokio-spawn` |
| Counter spawn pool drop semantics | under Redis stall, drops > 0 AND billing spills > 0 AND RSS < baseline+200 MB AND release spills > 0 | chaos `redis-stall.yaml` |
| Tokio runtime worker busy ratio under load | p99 < 0.85 | `make e2e-soak-smoke` + tokio_unstable metrics |
| `block_in_place` count in `gateway.rs` | `rg -c 'block_in_place' crates/gw-proxy/src/gateway.rs == 2` (both documented) | grep |
| Cross→same mid-stream switch byte fidelity | `cargo test -p gw-proxy --test e2e_cross_same_switch` passes | cargo |
| Detailed-log emit ratio | `gw_detailed_log_emit_total / gw_proxy_request_total >= 0.999` over 24h soak | Prom |
| Archive channel overflow alert | `promtool test` produces alert; `docs/runbooks/archive-channel-full.md` exists | promtool + ls |

All thresholds must be PASS to merge.

---

— end of WS-6 sub-plan —
