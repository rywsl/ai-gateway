# Task 1 Report: Rust Vendor Key Cost Weight

## What you implemented
- Added `VendorKey.model_cost_weights: HashMap<ModelName, u32>` in `gw-core` without adding `Deserialize` to `VendorKey`.
- Added `VendorKeyRow.model_cost_weights` in `gw-store::loader` with `#[serde(default)]` as the serde boundary for `vendor_keys.payload.model_cost_weights`.
- Passed the row field through `load_vendor_key` into `VendorKey`.
- Added in-file `VendorKeyRow` deserialization tests using the existing `api_key_enc` object wire shape.
- Added the requested `gw-core` struct-literal test for explicit model cost weights.
- Updated existing `VendorKey { ... }` literals to populate the new field. Copy helpers now clone existing `model_cost_weights` instead of dropping them.

## Tests run and results
- `cargo test -p gw-core --test vendor_key_identity vendor_key_struct_literal_sets_model_cost_weights -- --nocapture` - PASS, 1 passed.
- `cargo test -p gw-store --lib vendor_key_row -- --nocapture` - PASS, 9 passed.
- `cargo fmt --all -- --check` - PASS.
- `cargo check --workspace --lib --bins --locked` - PASS.
- `git diff --check` - PASS.

## TDD Evidence
### RED command/output summary
- `cargo test -p gw-core --test vendor_key_identity vendor_key_struct_literal_sets_model_cost_weights -- --nocapture`
  - Failed as expected with `error[E0560]: struct VendorKey has no field named model_cost_weights` and `error[E0609]: no field model_cost_weights on type VendorKey`.
- `cargo test -p gw-store --lib vendor_key_row_model_cost_weights -- --nocapture`
  - Failed as expected with `error[E0609]: no field model_cost_weights on type loader::VendorKeyRow` for both new row tests.

### GREEN command/output summary
- `cargo test -p gw-core --test vendor_key_identity vendor_key_struct_literal_sets_model_cost_weights -- --nocapture`
  - `test vendor_key_struct_literal_sets_model_cost_weights ... ok`; result: `1 passed; 0 failed`.
- `cargo test -p gw-store --lib vendor_key_row -- --nocapture`
  - New tests `vendor_key_row_model_cost_weights_default_to_empty` and `vendor_key_row_deserializes_model_cost_weights` passed; result: `9 passed; 0 failed`.

## Files changed
- `crates/gw-core/src/entity.rs`
- `crates/gw-core/tests/vendor_key_identity.rs`
- `crates/gw-store/src/loader.rs`
- `crates/gw-core/tests/capabilities.rs`
- `crates/gw-adapter/src/codex.rs`
- `crates/gw-adapter/src/kiro.rs`
- `crates/gw-adapter/src/vertex.rs`
- `crates/gw-adapter/tests/noop_behavior.rs`
- `crates/gw-proxy/src/gateway.rs`
- `crates/gw-proxy/src/filters/gemini_files.rs`
- `crates/gw-proxy/src/filters/logging.rs`
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-proxy/tests/billing_lifecycle.rs`
- `crates/gw-proxy/tests/capability_filter.rs`
- `crates/gw-routing/benches/strategy_pick.rs`
- `crates/gw-routing/src/candidates.rs`
- `crates/gw-routing/src/failover.rs`
- `crates/gw-routing/src/pipeline/multi_dim.rs`
- `crates/gw-routing/src/strategy/cost_first.rs`
- `crates/gw-routing/src/strategy/latency_first.rs`
- `crates/gw-routing/src/strategy/load_balance.rs`
- `crates/gw-routing/src/strategy/sticky.rs`
- `crates/gw-routing/tests/candidates_blacklist.rs`
- `crates/gw-routing/tests/pipeline_equivalence.rs`
- `crates/gw-routing/tests/reservation_gate.rs`
- `crates/gw-routing/tests/sticky_ttl.rs`
- `crates/gw-routing/tests/strategies.rs`

## Self-review findings
- Verified `VendorKey` still has no `Deserialize` derive.
- Verified `VendorKeyRow` owns the serde boundary and uses `#[serde(default)]` for `model_cost_weights`.
- Verified the row tests use `api_key_enc` as the existing encrypted object shape, not plaintext `api_key`.
- Found and fixed two copy helpers that initially defaulted `model_cost_weights`; they now clone `key.model_cost_weights` so loaded weights are preserved.
- Ran workspace lib/bin check to catch missed `VendorKey` literals beyond the targeted tests.

## Issues or concerns
- None.
