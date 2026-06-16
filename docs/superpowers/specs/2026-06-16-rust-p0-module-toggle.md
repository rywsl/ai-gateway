# Rust P0-C — runtime enforcement of `module.<name>.enabled` (API-key module toggle)

> Implementation spec. **Source of truth: the Codex analysis** (GitLab Go work item #26 / P0-C,
> reproduced at `work/claude-superpowers-router/20260616-024052/codex-analysis.md`).
> Scope: **Rust crates / docs / tests only**. Do **not** edit Go. Preserve the unrelated
> in-flight worktree changes (capability aliases, KEK command, vendor-key write contract).
>
> **Verification constraint:** the local host is Windows without the MSVC `link.exe` linker, so
> `cargo build` / `cargo test` cannot link here (toolchain limitation, not a code error; see the
> recorded handoff memory). All compile + test verification runs in CI / Codex. This spec therefore
> favours small, locally-reasoned, high-confidence edits and pure-logic unit tests.

## Reproduced gap (read before modifying)

Disabling a module in Go (`modules/system/business/modules.go::SetModuleEnabled`) writes a config
row keyed `module.<name>.enabled` and emits an invalidation event, proven by the Go test
`TestSetModuleEnabledEmitsConfigInvalidation`:

```text
stream = events:entity
ns     = config
id     = module.apikey.enabled
op     = upsert
```

Rust `crates/gw-store/src/stream_consumer.rs::apply_entity_event` accepts `ns="config"` but only
DELs the Redis L1 key `entity:config:<id>` (`stream_consumer.rs:159-162`); there is **no** runtime
cache, reload, or request-path enforcement. Repository search finds no Rust reader for
`module.apikey.enabled`. → Disabling the API-key module in Go changes the admin UI / PG state, but
direct API-key gateway traffic keeps flowing. The P0 claim remains true for runtime enforcement.

## Design — smallest robust runtime contract

Three pieces, no new bootstrap hook, no new Rust-owned migration.

### 1. Module-state reader + in-process cache (`gw-store`)

- **Pure parser** `module_enabled_from_value(raw: &str) -> bool` (in `loader.rs`). *Default-enabled*
  semantics: only an explicit, recognised "disabled" token returns `false`; everything else
  (including unparseable) returns `true`. Recognised disabled tokens (case-insensitive, after
  trimming surrounding whitespace and one layer of JSON quotes): `false`, `f`, `0`, `no`, `off`,
  `disabled`. This tolerates a JSONB column (`false`), a TEXT column (`"false"` / `false`), and a
  `boolean` column (`f`) — see the `value::text` cast below.
- **Loader** `load_module_enabled(ctx: &LoaderCtx, name: &str) -> anyhow::Result<bool>`:
  ```sql
  SELECT value::text FROM system_configs WHERE key = $1     -- $1 = "module.<name>.enabled"
  ```
  The `::text` cast makes the read **column-type agnostic** (works for `text` / `jsonb` /
  `boolean`). Returns `Ok(true)` when the row is absent or `value` is `NULL` (default-enabled,
  matching Go's default-enabled modules); `Ok(false)` only on an explicit disabled token; `Err`
  only on a DB/transport failure.
- **Cache** `EntityStore.module_enabled: Cache<String, bool>` (Moka, 300 s TTL — auth-surface
  bucket, matching `apikey`). Key = bare module name (`"apikey"`).
- **Accessor** `EntityStore::module_enabled_or_load(name: &str) -> bool`: Moka L1 → `try_get_with`
  dedup → `load_module_enabled` under the existing `with_cold_load_timeout`. **Fail-open** to `true`
  when no loader is wired (unit tests), on cold-load timeout, or on loader `Err` — and the `Err`
  result is **not** cached, so the next request retries. An explicit `false` *is* cached (until the
  invalidation event or TTL). This is the documented default-enabled contract.
- **Invalidator** `EntityStore::invalidate_module_enabled(name: &str)`.

Rationale for PG-direct (no Redis L1 envelope for module flags): Go writes only PG for module rows
(the reproduced gap), and the `system_configs` key/value shape does not fit the versioned
`entity:<ns>:<id>` envelope `{v,enc,d}`. Cross-pod coherence comes from the event-driven Moka
invalidation below (every pod consumes the event and invalidates its own L1), not from a shared
Redis entry. The existing `entity:config:<id>` Redis DEL is left intact (harmless for module ids).

### 2. Event-driven invalidation (`gw-store::stream_consumer`)

Extend the existing `"config"` arm of `apply_entity_event`. A helper
`module_name_from_config_id(id) -> Option<&str>` recognises `module.<name>.enabled` (strip the
`module.` prefix and `.enabled` suffix; `<name>` must be non-empty). When it matches, call
`store.invalidate_module_enabled(name)` **in addition to** the existing Redis L1 DEL. Non-module
`config` ids keep the current behaviour (Redis L1 DEL only). This satisfies:
*`events:entity ns=config id=module.apikey.enabled` invalidates the runtime state so a subsequent
request does not use stale enabled state.*

### 3. Request-path enforcement (`gw-proxy::filters::request`)

Single chokepoint, immediately after the auth branches resolve
`let (user_id, api_key_id, ip_allowlist_for_request) = user_id_res?;` (`request.rs:375`):

```rust
// P0-C: API-key module toggle. `api_key_id` is Some only for the two API-key
// auth shapes; the JWT branch returns None, and management-token auth never
// reaches here — so JWT/admin paths are unaffected. Default-enabled: a missing
// row or an unreadable config store leaves the module enabled (fail-open read),
// while an explicit `module.apikey.enabled=false` fails closed.
if apikey_module_blocks(api_key_id.as_ref(), store.module_enabled_or_load("apikey").await) {
    let err = ErrorIR { /* ClientAuth, 403, GW_AUTH_MODULE_DISABLED, "api key module disabled" */ };
    bail!(session, ctx, &err, &request_id)
}
```

- Pure predicate `apikey_module_blocks(api_key_id: Option<&ApiKeyId>, enabled: bool) -> bool`
  (= `api_key_id.is_some() && !enabled`) extracted alongside the existing `check_*` pure helpers so
  it can be unit-tested without a `Session`. Mirrors the `check_api_key_status` test pattern.
- New stable code `GW_AUTH_MODULE_DISABLED` in `gw-core::error::codes`. **HTTP 403** (the credential
  is valid; the auth *method* is administratively disabled — a policy/forbidden condition, distinct
  from `GW_AUTH_KEY_DISABLED`/401 which is a per-key state and from `GW_AUTH_INVALID_APIKEY`/401
  which is a bad credential). Placed after successful authentication so it never leaks module state
  to unauthenticated callers.

## Files changed

| File | Change |
|------|--------|
| `crates/gw-core/src/error/codes.rs` | add `GW_AUTH_MODULE_DISABLED` |
| `crates/gw-store/src/loader.rs` | add `module_enabled_from_value`, `load_module_enabled`, `MODULE_ENABLED_SQL`; parser unit tests |
| `crates/gw-store/src/entity_store.rs` | add `module_enabled` cache + builder entry + `module_enabled_or_load` + `invalidate_module_enabled` |
| `crates/gw-store/src/stream_consumer.rs` | extend `"config"` arm + `module_name_from_config_id` helper; expand `config_tests` |
| `crates/gw-proxy/src/filters/request.rs` | enforcement chokepoint + `apikey_module_blocks` pure predicate + unit test |
| `docs/go-contracts/parts/08-control-plane.md` | document `ns=config id=module.<name>.enabled` + `system_configs` read contract |
| `docs/architecture.md` | §Deliberate behaviours: fail-closed API-key module toggle |
| `docs/spec/05-auth-permissions.md` | new module-toggle gate |

## Tests (TDD; run in CI / Codex — local link.exe blocked)

1. `loader.rs` parser: `true`/`"true"`/`1`/`t`/`yes`/`on`/absent → enabled; `false`/`"false"`/`f`/`0`/`no`/`off`/`disabled` → disabled; whitespace + quote tolerance.
2. `entity_store.rs`: `module_enabled_or_load` with no loader → `true` (default-enabled).
3. `stream_consumer.rs`: pre-seed `module_enabled` cache `true`, fire `apply_entity_event("config","module.apikey.enabled")`, assert the cache entry is gone; assert a non-module `config` id does **not** touch the cache; `module_name_from_config_id` unit cases.
4. `request.rs`: `apikey_module_blocks` truth table (`Some`+disabled → block; `Some`+enabled → allow; `None`+disabled → allow — JWT unaffected).

## Acceptance criteria mapping

- *Rust can observe `module.apikey.enabled=false` and fail closed* → loader + `module_enabled_or_load` + request chokepoint.
- *Event invalidates runtime state* → `stream_consumer` `"config"` arm.
- *Missing row ⇒ enabled* → parser/loader default + fail-open accessor.
- *auth_config hot reload intact* → `"auth_config"` arm untouched; new logic is additive.
- *No unrelated changes overwritten* → edits are additive; capability/KEK/vendor-key worktree diffs left as-is.

## Non-goals / constraints honoured

- No Rust-owned `system_configs` migration (Go owns the rows). Rust only *reads*; the read contract
  is documented in `parts/08` for Codex to reconcile against Go's actual schema.
- No `kek_keys` table; no KEK design change.
- No broad control-plane/`ConfigReloadRegistry` rewrite (that registry is not yet built in Rust).
- No commit, no push.

## Known risk for Codex review

The authoritative `system_configs` table/column names (`key`, `value`) are an **assumed contract** —
the Go source was not reachable from this worktree to confirm. If Go's actual shape differs, the
cold-load returns `Err` and **fails open** (module stays enabled) — enforcement then silently
no-ops rather than breaking traffic. Codex should confirm the table/column names against Go and
reconcile `MODULE_ENABLED_SQL` + `parts/08` if they differ. The `value::text` cast already absorbs
any `text`/`jsonb`/`boolean` column-type difference.
