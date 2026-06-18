# R4 SETTLE_LUA Idempotency Report

## Files Changed

- `crates/gw-task/src/scripts.rs`
- `crates/gw-task/src/redis_contract.rs`
- `crates/gw-task/tests/scripts.rs`

## Red Command / Output Summary

Command:

```bash
cargo test -p gw-task test_settle_lua_terminal_status_guard_precedes_mutations
```

Result: failed as expected.

Key output:

```text
test test_settle_lua_terminal_status_guard_precedes_mutations ... FAILED
settle lua must read existing task status before mutating
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3 filtered out
```

## Green Command / Output Summary

Command:

```bash
cargo test -p gw-task test_settle_lua_terminal_status_guard_precedes_mutations
```

Result: passed.

Key output:

```text
test test_settle_lua_terminal_status_guard_precedes_mutations ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out
```

Formatting check:

```bash
cargo fmt --all --check
```

Result: passed with no output.

## Concerns

- This change follows the requested R4 / M5-only scope: it adds the `settled` / `cancelled` / `expired` guard in `SETTLE_LUA` before `HSET`, `INCRBY`, and `XADD`, and preserves the original mutation order after the guard.
- The caller-side `noop` handling described in M5 was not changed because this task explicitly limited ownership to `gw-task/src/scripts.rs` and gw-task tests.
- Controller reconciliation added a caller-side guard so `RedisTaskContract::settle` treats Lua `noop` as idempotent success instead of a storage error.
- Additional red/green:
  - Red: `cargo test -p gw-task test_settle_caller_accepts_noop_as_idempotent_success` failed on the old `result != "ok"` check.
  - Green: `cargo test -p gw-task --test scripts` passed 5/5.
