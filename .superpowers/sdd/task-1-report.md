# Task 1 Report: Rust Active Registration Infrastructure

## Summary

Implemented gw-ledger active-registration infrastructure:

- Added `ProtectionConfig` active-registration defaults:
  - `active_registration_ttl_secs = 300`
  - `active_registration_refresh_min_interval_secs = 30`
  - `active_registration_queue_capacity = 8192`
- Added active registration metric constants verbatim from the brief.
- Added `active_registration` module with:
  - `ACTIVE_USERS_KEY = "credit:active_users"`
  - `ACTIVE_USER_PREFIX = "credit:active_user:"`
  - `active_user_key`
  - `ActiveRegistrationEnqueueResult`
  - `ActiveRegistrationHandle`
  - `spawn_active_registration_worker`
  - Redis Lua registration write path.
- Exported `active_user_key`, `ActiveRegistrationEnqueueResult`, and `ActiveRegistrationHandle` from `gw-ledger`.

## TDD RED

### Protection config RED

Command:

```bash
cargo test -p gw-ledger protection_config -- --nocapture
```

Result: failed as expected before implementation.

Summary:

```text
error[E0609]: no field `active_registration_ttl_secs` on type `protection_config::ProtectionConfig`
error[E0609]: no field `active_registration_refresh_min_interval_secs` on type `protection_config::ProtectionConfig`
error[E0609]: no field `active_registration_queue_capacity` on type `protection_config::ProtectionConfig`
error: could not compile `gw-ledger` (lib test) due to 6 previous errors
```

### Active registration RED

Command:

```bash
cargo test -p gw-ledger active_registration -- --nocapture
```

Result: failed as expected before implementation because the module API was absent.

Summary:

```text
error[E0422]: cannot find struct, variant or union type `ActiveRegistrationEvent` in this scope
error[E0425]: cannot find value `ACTIVE_USERS_KEY` in this scope
error[E0433]: failed to resolve: use of undeclared type `ActiveRegistrationHandle`
error[E0433]: failed to resolve: use of undeclared type `ActiveRegistrationEnqueueResult`
error[E0425]: cannot find function `write_registration_once` in this scope
error[E0425]: cannot find function `active_user_key` in this scope
error: could not compile `gw-ledger` (lib test) due to 16 previous errors
```

The brief test snippet needed two local API adjustments:

- Imported `fred::interfaces::ClientLike` so `RedisPool::init()` is in scope.
- Replaced `UserId::new(raw.to_string()).expect("user id")` with the repo's existing constructor pattern, `UserId::from(raw)`.

Behavior is unchanged: tests still create the same logical user IDs and initialize the same fred Redis pool.

## GREEN / Verification

### Protection config GREEN

Command:

```bash
cargo test -p gw-ledger protection_config -- --nocapture
```

Result:

```text
running 2 tests
test protection_config::tests::default_enables_hard_mode_protection ... ok
test protection_config::tests::deserialize_partial_overrides_keep_defaults ... ok
test result: ok. 2 passed; 0 failed
```

### Metric/config compile check

Command:

```bash
cargo check -p gw-ledger
```

Result:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s)
```

### Active registration unit tests

Commands:

```bash
cargo test -p gw-ledger active_registration::tests::disabled_handle_never_queues -- --nocapture
cargo test -p gw-ledger active_registration::tests::handle_throttles_same_user_inside_interval -- --nocapture
cargo test -p gw-ledger active_registration::tests::queue_full_is_non_fatal -- --nocapture
```

Result:

```text
test active_registration::tests::disabled_handle_never_queues ... ok
test active_registration::tests::handle_throttles_same_user_inside_interval ... ok
test active_registration::tests::queue_full_is_non_fatal ... ok
```

### Full active registration command

Command:

```bash
cargo test -p gw-ledger active_registration -- --nocapture
```

Result: compile succeeded; 3 pure unit tests passed; Redis integration test could not start because Docker is not reachable in this environment.

Summary:

```text
running 4 tests
test active_registration::tests::disabled_handle_never_queues ... ok
test active_registration::tests::handle_throttles_same_user_inside_interval ... ok
test active_registration::tests::queue_full_is_non_fatal ... ok
thread 'active_registration::tests::write_registration_sets_membership_marker_and_ttl' panicked:
redis: Client(Init(SocketNotFoundError("/var/run/docker.sock")))
test result: FAILED. 3 passed; 1 failed
```

Docker environment check:

```bash
docker info --format '{{.ServerVersion}}'
```

Result:

```text
failed to connect to the docker API at unix:///Users/whereslow/.docker/run/docker.sock
```

Also verified:

```bash
cargo fmt --all -- --check
```

Result: passed.

## Concerns

The Redis-backed integration test is not green locally because the Docker socket target does not exist:

```text
/var/run/docker.sock -> /Users/whereslow/.docker/run/docker.sock
/Users/whereslow/.docker/run/docker.sock: No such file or directory
```

No production code outside the task-owned files was modified.

## Review Fix: Active Registration Atomic Reservation

Fixed review findings in `crates/gw-ledger/src/active_registration.rs`:

- Replaced check-then-insert same-user throttling with a DashMap entry-based reservation before enqueue.
- Roll back a failed reservation on `Full` or `Closed` only when the stored value still equals the attempted `admitted_at_ms`.
- Added `queue_capacity` to `ActiveRegistrationHandle` and changed `CREDIT_ACTIVE_REGISTER_QUEUE_DEPTH` to report actual queue depth as `capacity - tx.capacity()`.
- Disabled and closed-without-sender paths no longer write a nonsensical queue depth gauge value.

### RED Evidence

Command:

```bash
cargo test -p gw-ledger active_registration::tests::concurrent_same_user_activity_queues_once_inside_interval -- --nocapture
```

Result: failed as expected before implementation because the old check-then-insert throttle admitted multiple concurrent same-user calls.

Summary:

```text
running 1 test
thread 'active_registration::tests::concurrent_same_user_activity_queues_once_inside_interval' panicked:
assertion `left == right` failed: results=[Throttled, Throttled, Queued, Queued, ...]
  left: 6
 right: 1
test active_registration::tests::concurrent_same_user_activity_queues_once_inside_interval ... FAILED
test result: FAILED. 0 passed; 1 failed
```

Rollback guard command:

```bash
cargo test -p gw-ledger active_registration::tests::full_queue_rolls_back_same_user_reservation -- --nocapture
```

Result before implementation: passed on the old implementation because it inserted only after successful send. The test remains as a regression guard for the new reserve-before-send behavior.

Summary:

```text
running 1 test
test active_registration::tests::full_queue_rolls_back_same_user_reservation ... ok
test result: ok. 1 passed; 0 failed
```

### GREEN Evidence

Commands:

```bash
cargo test -p gw-ledger active_registration::tests::concurrent_same_user_activity_queues_once_inside_interval -- --nocapture
cargo test -p gw-ledger active_registration::tests::full_queue_rolls_back_same_user_reservation -- --nocapture
cargo test -p gw-ledger active_registration::tests::disabled_handle_never_queues -- --nocapture
cargo test -p gw-ledger active_registration::tests::handle_throttles_same_user_inside_interval -- --nocapture
cargo test -p gw-ledger active_registration::tests::queue_full_is_non_fatal -- --nocapture
cargo fmt --all -- --check
```

Result:

```text
test active_registration::tests::concurrent_same_user_activity_queues_once_inside_interval ... ok
test active_registration::tests::full_queue_rolls_back_same_user_reservation ... ok
test active_registration::tests::disabled_handle_never_queues ... ok
test active_registration::tests::handle_throttles_same_user_inside_interval ... ok
test active_registration::tests::queue_full_is_non_fatal ... ok
cargo fmt --all -- --check: passed
```

Full active-registration filter was also rerun:

```bash
cargo test -p gw-ledger active_registration -- --nocapture
```

Result: the five non-Docker active-registration tests passed; the existing Redis integration test still cannot start because Docker is unavailable in this environment.

Summary:

```text
running 6 tests
test active_registration::tests::disabled_handle_never_queues ... ok
test active_registration::tests::full_queue_rolls_back_same_user_reservation ... ok
test active_registration::tests::queue_full_is_non_fatal ... ok
test active_registration::tests::handle_throttles_same_user_inside_interval ... ok
test active_registration::tests::concurrent_same_user_activity_queues_once_inside_interval ... ok
thread 'active_registration::tests::write_registration_sets_membership_marker_and_ttl' panicked:
redis: Client(Init(SocketNotFoundError("/var/run/docker.sock")))
test result: FAILED. 5 passed; 1 failed
```
