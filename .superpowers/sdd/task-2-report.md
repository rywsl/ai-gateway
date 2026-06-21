# Task 2 Report: Rust Admission Fallback and Registration Wiring

## Summary

Implemented Task 2 wiring in the Rust gateway path:

- Added `OverflowGate::fallback_balance_micro(&self, user_id: &str) -> Result<i64, OverflowError>`.
  - Reuses the existing `balance_for` cache/PG fallback path.
  - Records `gw_credit_balance_fallback_total` for positive/zero outcomes with the source path label.
- Wired `gw_ledger::ActiveRegistrationHandle` through:
  - `gw-gateway` bootstrap worker spawn and shutdown registration.
  - `Wired` -> `LlmGateway::new` -> `filters::request::run`.
- Added cold counter fallback admission for normal LLM requests only.
  - Redis counter balance remains primary.
  - If counter-derived available balance is `<= 0`, normal LLM requests consult `OverflowGate::fallback_balance_micro`.
  - Positive fallback balance proceeds through the existing cold `RequestFreeze` path.
  - Gemini Files and task paths do not use fallback.
  - Request code records only fallback `error`; positive/zero metrics are recorded by `OverflowGate` to avoid double-counting.
- Registered admitted normal activity only after admitted paths:
  - protected lease acquire success.
  - overflow-admitted `Expired`, `Insufficient`, and protected `NoLease` paths.
  - cold `RequestFreeze` success.
- Did not register balance rejection, Gemini Files, task, hard-cap rejection, or freeze construction failure.
- Hot request admission calls only `ActiveRegistrationHandle::try_enqueue_activity`; no Redis await or per-request spawn was added for active registration.

## Files Changed

- `crates/gw-ledger/src/overflow.rs`
- `crates/gw-proxy/src/gateway.rs`
- `crates/gw-proxy/src/filters/request.rs`
- `crates/gw-gateway/src/lib.rs`

No Task 3 config/sweep/docs files were modified.

## TDD RED Evidence

### Overflow fallback API RED

Command:

```bash
cargo test -p gw-ledger fallback_balance_micro_reuses_balance_for_path -- --nocapture
```

Result before implementation: failed as expected because the method did not exist.

Summary:

```text
error[E0599]: no method named `fallback_balance_micro` found for struct `overflow::OverflowGate` in the current scope
error: could not compile `gw-ledger` (lib test) due to 1 previous error
```

### Active-registration source-order RED

Initial version of the source-order test incorrectly passed because it matched the test module's own `register_admitted_activity(` strings. I tightened it to scan only production source before `#[cfg(test)]`, then reran.

Command:

```bash
cargo test -p gw-proxy active_registration_is -- --nocapture
```

Result before implementation: failed as expected because production code had no registration call sites.

Summary:

```text
running 2 tests
test filters::request::active_registration_source_order_tests::active_registration_is_not_inside_task_or_gemini_files_short_circuit ... ok
thread 'filters::request::active_registration_source_order_tests::active_registration_is_after_admitted_normal_paths' panicked:
assertion failed: register.iter().any(|idx| *idx > lease_success)
test result: FAILED. 1 passed; 1 failed
```

## GREEN / Verification Evidence

### Proxy active-registration source-order tests

Command:

```bash
cargo test -p gw-proxy active_registration_is -- --nocapture
```

Result:

```text
running 2 tests
test filters::request::active_registration_source_order_tests::active_registration_is_not_inside_task_or_gemini_files_short_circuit ... ok
test filters::request::active_registration_source_order_tests::active_registration_is_after_admitted_normal_paths ... ok
test result: ok. 2 passed; 0 failed
```

### Proxy + gateway compile check

Command:

```bash
cargo check -p gw-proxy -p gw-gateway
```

Result:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s)
```

### Formatting

Command:

```bash
cargo fmt --all -- --check
```

Result: passed.

### Overflow fallback test after implementation

Command:

```bash
cargo test -p gw-ledger fallback_balance_micro_reuses_balance_for_path -- --nocapture
```

Result: compiled through the new API, then failed at Docker startup because this environment has no Docker socket.

Summary:

```text
running 1 test
thread 'overflow::tests::fallback_balance_micro_reuses_balance_for_path' panicked:
redis: Client(Init(SocketNotFoundError("/var/run/docker.sock")))
test result: FAILED. 0 passed; 1 failed
```

Docker check:

```bash
docker info --format '{{.ServerVersion}}'
```

Result:

```text
failed to connect to the docker API at unix:///Users/whereslow/.docker/run/docker.sock; check if the path is correct and if the daemon is running: dial unix /Users/whereslow/.docker/run/docker.sock: connect: no such file or directory
```

## Constructor/Test Adjustments

No nearby production test constructors needed direct signature updates. The only constructor changes were production wiring from `gw-gateway` into `gw-proxy::LlmGateway::new` and then into `filters::request::run`.

## Concerns

The Docker-backed `gw-ledger` fallback integration test cannot run in this environment because Docker is unavailable. Non-Docker focused checks and compile verification pass.
