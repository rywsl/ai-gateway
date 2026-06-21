# Task 3 Report: Rust Lease Sweep, Config File, Metrics Catalog, and Docs

## Summary

Implemented local protected lease cleanup in Rust, wired the sweep loop to `[protection]` config, added default active-registration config values, registered the Task 1-2 Rust metrics in the gateway catalog, and updated Rust/Go contract docs.

The brief's simple `ledger_sweep::spawn(shutdown, protection_cfg)` snippet was adapted to the local codebase: `ledger_sweep` is already owned by `gw_runtime::Supervisor`, so the change preserves the existing supervised shutdown/restart pattern and extends its signature to `spawn(shutdown, protection_cfg)`.

`crates/gw-gateway/src/config.rs` did not require a code change because it already exposes `pub protection: gw_ledger::ProtectionConfig`.

## RED Evidence

Command:

```bash
cargo test -p gw-ledger sweep_ -- --nocapture
```

Result: failed as expected after replacing the protected-entry no-op invariant with the new sweep tests.

Key output:

```text
error[E0061]: this function takes 0 arguments but 3 arguments were supplied
   --> crates/gw-ledger/src/lease.rs:373:30
    |
373 |         let removed_before = sweep_leases(1_000 + 30_000 + 300_000, 30_000, 300);
    |                              ^^^^^^^^^^^^

error[E0061]: this function takes 0 arguments but 3 arguments were supplied
   --> crates/gw-ledger/src/lease.rs:377:29
    |
377 |         let removed_after = sweep_leases(1_000 + 30_000 + 300_000 + 1, 30_000, 300);
    |                             ^^^^^^^^^^^^

error[E0425]: cannot find function `sweep_cutoff_ms` in this scope
   --> crates/gw-ledger/src/lease.rs:384:20
    |
384 |         assert_eq!(sweep_cutoff_ms(u64::MAX - 5, 30_000, 300), u64::MAX);
    |                    ^^^^^^^^^^^^^^^ not found in this scope
```

## Implementation Notes

- Added `sweep_cutoff_ms(expires_at_ms, lease_grace_ms, active_registration_ttl_secs)` using saturating arithmetic.
- Changed `sweep_leases(now_ms, lease_grace_ms, active_registration_ttl_secs)` to retain entries only while:

```text
now_ms <= expires_at_ms + lease_grace_ms + active_registration_ttl_secs * 1000
```

- Incremented `gw_credit_lease_sweep_removed_total` only when entries are removed.
- Updated `ledger_sweep::spawn` to accept `ProtectionConfig` while preserving the existing `Supervisor` wrapper and shutdown token behavior.
- Updated gateway bootstrap to call `gw_ledger::ledger_sweep::spawn(shutdown_token.clone(), cfg.protection.clone())`.
- Added default TOML values:

```toml
active_registration_ttl_secs = 300
active_registration_refresh_min_interval_secs = 30
active_registration_queue_capacity = 8192
```

- Added metrics catalog entries:
  - `gw_credit_active_register_enqueue_total`
  - `gw_credit_active_register_redis_total`
  - `gw_credit_active_register_queue_depth`
  - `gw_credit_lease_sweep_removed_total`
  - `gw_credit_balance_fallback_total`

## GREEN Evidence

Command:

```bash
cargo test -p gw-ledger sweep_ -- --nocapture
```

Result:

```text
running 4 tests
test lease::tests::sweep_cutoff_saturates_on_large_values ... ok
test coef::tests::sweep_orphans_noop_on_empty_staging ... ok
test coef::tests::sweep_orphans_evicts_versions_below_active_minus_margin ... ok
test coef::tests::sweep_orphans_noop_when_active_below_margin ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 97 filtered out
```

The brief-provided protected-entry test name does not include `sweep_`, so I also ran it directly:

```bash
cargo test -p gw-ledger protected_entries_are_swept_after_grace_and_active_ttl -- --nocapture
```

Result:

```text
running 1 test
test lease::tests::protected_entries_are_swept_after_grace_and_active_ttl ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 100 filtered out
```

Compile checks:

```bash
cargo check -p gw-ledger -p gw-gateway
```

Result:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s)
```

```bash
cargo check -p gw-gateway
```

Result:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s)
```

Formatting:

```bash
cargo fmt --all -- --check
```

Result: exit 0.

Catalog/docs/config evidence:

```bash
rg -n "active_registration|credit:active_users|gw_credit_active_register" docs crates/gw-gateway/src/metrics_catalog.rs config/default.toml
```

Result included:

```text
crates/gw-gateway/src/metrics_catalog.rs:728:        name: "gw_credit_active_register_enqueue_total",
crates/gw-gateway/src/metrics_catalog.rs:733:        name: "gw_credit_active_register_redis_total",
crates/gw-gateway/src/metrics_catalog.rs:738:        name: "gw_credit_active_register_queue_depth",
config/default.toml:244:active_registration_ttl_secs = 300
config/default.toml:245:active_registration_refresh_min_interval_secs = 30
config/default.toml:246:active_registration_queue_capacity = 8192
docs/spec/06-billing-ledger.md:91:- `LEASE_POOL` protected entries only after `now_ms > expires_at_ms + lease_grace_ms + active_registration_ttl_secs * 1000`, so a recently active Rust request keeps enough time for Go renewal before local cleanup.
docs/spec/06-billing-ledger.md:95:After a normal billable LLM proxy request is admitted, Rust enqueues an active-registration event without awaiting Redis. The worker atomically writes `credit:active_users` and `credit:active_user:<uid>` so Go can renew leases for recently active funded users. Task endpoints and Gemini Files endpoints do not write active registration in this phase.
docs/go-contracts/main.md:97:Rust writes `credit:active_users` and `credit:active_user:<uid>` with marker value `<last_seen_ms>|<pod_id>`. Go treats the marker key as authoritative liveness and may remove stale set members when the marker is missing.
```

## Files Changed

- `crates/gw-ledger/src/lease.rs`
- `crates/gw-ledger/src/ledger_sweep.rs`
- `crates/gw-gateway/src/lib.rs`
- `config/default.toml`
- `crates/gw-gateway/src/metrics_catalog.rs`
- `docs/spec/06-billing-ledger.md`
- `docs/workflow.md`
- `docs/go-contracts/main.md`

## Concerns

None.
