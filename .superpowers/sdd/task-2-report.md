# Task 2 Report: Rust Cost-Aware Routing And Inline Multi-Dim

## What I implemented

- Added `gw_core::entity::MultiDimWeights`.
- Replaced public `StrategyKind::MultiDim(String)` with `StrategyKind::MultiDimPreset(String)` and `StrategyKind::MultiDimInline(MultiDimWeights)`.
- Kept `StrategyKind` manual `Deserialize` through a private wire enum, preserving the preset JSON shape `{"multi_dim":"balanced"}` and adding inline JSON object support.
- Updated routing feature extraction so `CandidateFeatures.cost_weight` uses `VendorKey.model_cost_weights[ctx.model]` with neutral `1000` fallback, multiplied by `pricing.model_markup`.
- Left `CostFirstPolicy.needed_dims` and `CostFirstPolicy.score` unchanged.
- Added `StrategyChoice::{Borrowed, Owned}` plus `StrategySet::strategy_for_kind`.
- Switched gw-proxy upstream dispatch to `strategy_for_kind`, so inline `multi_dim` creates an owned `MultiDim` strategy while existing strategies stay borrowed.
- Preserved unknown multi_dim preset fallback to `fixed_order`, including warn-once and `gw_routing_unknown_preset_total`.

## Tests run and results

- `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=target/codex-task2-red cargo test -p gw-routing --test strategies cost_first -- --nocapture` - PASS, 3 passed.
- `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=target/codex-task2-red cargo test -p gw-routing --test strategy_set multi_dim -- --nocapture` - PASS, 4 passed.
- `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=target/codex-task2-red cargo test -p gw-proxy upstream --lib -- --nocapture` - PASS, 33 passed.
- `cargo fmt --all` - PASS.
- `git diff --check` - PASS.

## TDD Evidence

### RED command/output summary

- Initial brief-shaped cost tests accidentally passed because their priority values aligned with the old `priority * markup` behavior. I tightened the test priorities before production changes so the old behavior was observable.
- RED cost command:
  - Command: `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=target/codex-task2-red cargo test -p gw-routing --test strategies cost_first -- --nocapture`
  - Result: FAILED as expected.
  - Summary: `cost_first_uses_provider_model_cost_weight_when_pricing_exists` picked `expensive` instead of `cheap`; `cost_first_missing_provider_model_cost_weight_uses_neutral_1000` picked `missing` instead of `explicit-cheap`.
- RED inline multi_dim command:
  - Command: `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=target/codex-task2-red cargo test -p gw-routing --test strategy_set multi_dim -- --nocapture`
  - Result: FAILED as expected.
  - Summary: compile errors for missing `MultiDimWeights`, missing `StrategyKind::{MultiDimPreset, MultiDimInline}`, and missing `StrategySet::strategy_for_kind`.

### GREEN command/output summary

- Command: `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=target/codex-task2-red cargo test -p gw-routing --test strategies cost_first -- --nocapture`
  - Result: PASS, 3 passed.
- Command: `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=target/codex-task2-red cargo test -p gw-routing --test strategy_set multi_dim -- --nocapture`
  - Result: PASS, 4 passed.
- Command: `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=target/codex-task2-red cargo test -p gw-proxy upstream --lib -- --nocapture`
  - Result: PASS, 33 passed.

## Files changed

- `Cargo.lock`
- `crates/gw-core/src/entity.rs`
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-routing/Cargo.toml`
- `crates/gw-routing/src/pipeline/mod.rs`
- `crates/gw-routing/src/strategy/set.rs`
- `crates/gw-routing/tests/strategies.rs`
- `crates/gw-routing/tests/strategy_set.rs`

## Self-review findings

- `CostFirstPolicy.needed_dims` and `CostFirstPolicy.score` were not changed.
- `StrategyKind` does not derive or implement `Serialize`.
- Preset wire shape remains `{"multi_dim":"balanced"}`.
- Unknown preset fallback path is preserved.
- No Go or frontend files were touched.
- `Cargo.toml` and `Cargo.lock` changes are necessary because `gw-routing` integration tests now deserialize `StrategyKind` JSON with `serde_json`; this is a dev-dependency only.
- Normal shared-target Cargo runs were interrupted/stuck behind unrelated shared build activity, so focused verification used `CARGO_TARGET_DIR=target/codex-task2-red`.

## Issues or concerns

- No functional concerns from focused verification.
- Full workspace tests were not run; only the GREEN commands from the task brief plus formatting and diff whitespace checks were run.
