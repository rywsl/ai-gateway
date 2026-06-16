# Rust P0 Unified — rotate-kek command path, vendor_keys VIEW compat, cooldown/health contract

> Implementation spec. **Source of truth: the Codex analysis** (CNxus-Rust #20 / #12 / #11, META #10).
> Scope: **Rust crates / docs / tests only**. Do **not** edit Go. Builds on the prior pass
> [`2026-06-13-p0-20-kek-signature-secret.md`](2026-06-13-p0-20-kek-signature-secret.md) (multi-KEK
> bootstrap + state-machine tests already landed) and **advances** its "keep rotate-kek disabled"
> decision per the new Codex acceptance A.
>
> **Verification constraint:** the local host is Windows without the MSVC `link.exe` linker, so
> `cargo build`/`cargo test` cannot link here (toolchain limitation, not a code error). All compile +
> test verification runs in CI / Codex. This spec therefore favours small, locally-reasoned,
> high-confidence edits and pure-logic unit tests.

## Work items addressed

| Item | Title | This pass |
|------|-------|-----------|
| CNxus-Rust #20 | KEK 轮换状态机三层锁死 | **A/B**: command-driven finalize of the existing dual-read state machine; explicit KEK-persistence contract |
| CNxus-Rust #11 | vendor_keys VIEW 不可写 | **C**: audit Rust writers + SQL-contract lock test + document Go-production writable-VIEW dependency |
| CNxus-Rust #12 | cooldown/health PG 列文档撒谎 | **D**: confirm runtime-only, pin with a no-PG-write contract test, close the go-contracts doc gap |
| (Codex local diff) | capability Go aliases | **E**: preserved unchanged |

## Reproduction evidence (read before modifying)

1. **rotate-kek hard-disabled** — `crates/gw-gateway/src/control_plane/command_consumer.rs:198-206`
   lumps `RotateKek` with the other unimplemented actions and returns
   `command_disabled:rotate-kek`. The capability advertisement
   `crates/gw-core/src/control_plane.rs:402` is `disabled_command(RotateKek, "kek_rotation_command_hook_missing")`,
   pinned by `crates/gw-core/tests/control_plane_wire_lock.rs:114-118`.
2. **The state machine already exists and is safe** — `crates/gw-store/src/encrypt.rs:143-200`
   implements `enter_dual_read` / `mark_reencrypting` / `drop_old_kek` with fail-closed guards
   (max 2 KEKs, never drop active/only KEK, no double dual-read), and bootstrap already enters
   dual-read when a previous KEK is configured (`crates/gw-gateway/src/kms.rs:47-58`). What is
   missing is a runtime command to *finalize* a rotation (drop the old KEK) without a rolling
   restart, plus an observability flag/status. `CommandConsumer` does **not** hold a `KekCache`
   today (`command_consumer.rs:25-34`).
3. **vendor_keys writers** — three Rust paths write `vendor_keys`:
   `oauth_refresh.rs:39-46` (`jsonb_set(payload,'{oauth}')` + version CAS),
   `identity_persist.rs:98-107` (`SET identity_snapshot=... WHERE identity_snapshot IS NULL`),
   `quota_state.rs:52-70` (`SET quota_* = ...`, `jsonb_set(payload,'{quota_state}')`, version bump).
   In the Rust migrations `vendor_keys` is a **TABLE** (`0001` + `0009` + `0010`), so these are valid
   locally. Per the Codex analysis, **Go production** instead exposes `vendor_keys` as a writable
   **VIEW over `provider_keys`** (Go migration `00060_vendor_keys_write_compat.sql`). The Rust-side
   cross-process contract (`docs/go-contracts/parts/03-shared-schema.md:22-37`) never states this,
   so the column/SQL surface Rust depends on is undocumented and untested against the VIEW shape.
4. **cooldown/health is runtime-only** — `crates/gw-health/src/format_health.rs` keeps circuit +
   cooldown purely in atomics; `crates/gw-health/src/lib.rs` module doc says "No persistence in v1";
   `docs/spec/03-key-health.md:6,47` and `docs/architecture.md:229` (`key_health:*`, `circuit:*`
   "Reserved for Phase 5+", i.e. unused) agree. **No PG column** for cooldown/health exists or should.
   `parts/03-shared-schema.md` lists `vendor_keys` columns but says nothing about key-health, leaving
   room for a reader to assume persistence — the gap #12 targets.

## Design & changes

### A — rotate-kek command drives the in-process state machine (no faking, no KEK over the wire)

The command becomes genuinely executable for the operations the in-process `KekCache` can safely
perform. It carries a `step` arg:

- `status` — read-only: report `state` (`healthy` / `dual_read` / `dual_read_reencrypting`),
  `active_kid`, `resident_kid_count`. Never returns key material.
- `mark-reencrypting` — drive `KekCache::mark_reencrypting()` (informational flag; always safe).
- `drop-old` — require `old_kid`; drive `KekCache::drop_old_kek(old_kid)`. Fail-closed via the
  existing guards (refuses unless in dual-read, refuses to drop the active/only KEK). This lets an
  operator **finalize** a rotation — return to `Healthy(K_new)` — without the rolling restart that
  was previously the only way to drop the previous KEK.

Explicitly **rejected** over the command channel:

- `enter-dual-read` / supplying new KEK material → returns `kek_enter_dual_read_requires_bootstrap`.
  New KEK material must arrive via `GW_KMS__KEK_HEX` / `GW_KMS__PREVIOUS_KEK_HEX` at boot. The command
  channel rides the `events:entity` Redis trust boundary and must **never** transport KEK plaintext.

This does **not** fake re-encryption: dropping the old KEK is the operator's existing responsibility
after the Go/ops re-encryption batch completes (identical risk profile to the documented
restart-based drop in `parts/03 §3.5.3`); the command merely removes the restart requirement.

Edits:
- `command_consumer.rs`: add `pub kek: KekCache` field; add `fn rotate_kek(&self, args) -> CommandOutcome`;
  move `RotateKek` from the disabled arm to `dispatch`. Parse args via a `RotateKekArgs { step, old_kid }`
  struct. No plaintext KEK logging.
- `control_plane/mod.rs`: `start(...)` gains a `kek: KekCache` param, forwarded into `CommandConsumer`.
- `gateway/src/lib.rs`: pass `loader.kek.clone()` into `control_plane::start` (same clone pattern as
  `oauth_kek` at `lib.rs:707`; the clone shares one `Arc` so the command observes the same cache the
  hot-path decrypt uses).
- `gw-core/src/control_plane.rs:402`: `disabled_command(RotateKek, ...)` → `enabled_command(RotateKek)`.
- `control_plane_wire_lock.rs`: update the `rotate-kek` entry to `{"action":"rotate-kek","enabled":true}`.
- Unit tests in `command_consumer.rs`: status on healthy cache; mark-reencrypting transition;
  drop-old finalize back to healthy; drop-old refused when not dual-read; enter-dual-read rejected;
  bad args → `invalid_args:`.

### B — KEK persistence contract is made explicit (documentation, by design)

KEK material is **env-seeded, in-process only** (`KekCache` from `GW_KMS__KEK_HEX` /
`GW_KMS__PREVIOUS_KEK_HEX`). There is deliberately **no durable `kek_keys` table**: persisting KEK
material in PG/Redis would be a security regression and would collide with Go migration ownership.
Document this contract and the new command-driven finalize in `parts/03 §3.5.3`, `architecture.md §4.3`,
and the sp-c design doc. No schema, no migration.

### C — vendor_keys writable-VIEW compatibility (audit + lock test + contract)

- Audit confirms the three writers target only columns/JSONB the contract already assigns to the
  Rust runtime writer (`oauth`, `identity_snapshot`, `quota_*`, `payload`, `version`, `updated_at`).
- Add a pure SQL-contract lock test (no DB) asserting each writer's SQL targets `vendor_keys`, touches
  exactly the agreed column set, and keeps `WHERE ... deleted_at IS NULL` / version semantics — so a
  future silent change to the column surface breaks the test and forces a go-contracts update.
- Document in `parts/03 §3.2.5` / a new note: Go production exposes `vendor_keys` as a **writable VIEW
  over `provider_keys`** (`00060_vendor_keys_write_compat.sql`); that VIEW (via `INSTEAD OF` triggers or
  updatable-view rules) **must** cover every column/JSONB path the Rust writers use, listed explicitly.
  Rust does not change its SQL — the contract pins what Go must keep writable.

### D — cooldown/health runtime-only contract (close the doc gap + lock test)

- Add an explicit statement to `parts/03` that key-health circuit/cooldown state is **per-replica,
  in-memory, never persisted**, and `vendor_keys` carries **no** cooldown/health columns (only the
  `quota_*` runtime exception already documented). Mirrors `spec/03-key-health.md §47`.
- Add a `gw-store` test asserting no writer emits a cooldown/health column (guards against a future
  writer inventing a non-existent Go-owned column — the failure mode #12 named).

### E — preserve Codex capability aliases

`crates/gw-capability/src/resolver.rs` Go-projection aliases (`extended_thinking`/`function_calling`/
`streaming`) and their test rows are **left unchanged**.

## Backward compatibility

- **Go migrations / deployments**: no new Rust migration; no SQL change to the vendor_keys writers.
  rotate-kek `enabled:true` is a capability-advertisement change Go consumes — synced in
  `parts/08 §8.4.3` with the new `step`/`old_kid` args. The command is additive and fail-closed;
  a Go side that never sends it is unaffected.
- **Mid-rotation restart** still works (bootstrap dual-read unchanged). `drop-old` via command and via
  restart are equivalent and idempotent (dropping a non-resident kid is refused, not destructive).

## Non-goals

- No at-rest re-encryption worker (operator/Go owns the batch).
- No `enter-dual-read` / KEK material over the command channel.
- No durable KEK table; no `signature_secret` envelope (prior pass; Go-coordinated).
- No routing/proxy refactor; no removal of merged CI mirror fixes; no commits; no GitLab push.

## Verification (run in CI / Codex; blocked locally by missing MSVC `link.exe`)

```bash
cargo fmt --all -- --check
cargo test -p gw-core --test control_plane_wire_lock
cargo test -p gw-store --lib
cargo test -p gw-gateway control_plane --lib
cargo test -p gw-capability intern_tests --lib
bash scripts/check-invariants.sh
```
