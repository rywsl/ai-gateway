# Implementation spec — control-plane `reset-cost` command (P0 #18 pool runtime state)

Date: 2026-06-13 · Source of truth: Codex analysis "P0 #18 Pool Runtime State" · Scope: Rust gateway only (Go side handled by Codex).

## Problem

The Go key-pool admin surface exposes three operations — 清冷却 (clear cooldown), 恢复 (recover), 重置 (reset). Go now issues `clear-cooldown` for clear-cooldown/recover via `contracts.GatewayCommandIssuer`. Rust already executes `clear-cooldown`. The 重置 (reset) action maps to a `reset-cost` command that **Rust does not yet support**, so the admin "重置" button is a no-op end-to-end.

## Key finding: Rust holds no per-key cost accumulator

CLAUDE.md is explicit: **cost computation is not performed in Rust** (the Go billing service consumes `usage:records`; the `no_cost_micro`/`no_billing_crate` lints enforce this). Confirmed in code:

- `CostFirst` strategy scores by `priority × model_markup` only (`crates/gw-routing/src/strategy/cost_first.rs`).
- `CandidateFeatures.cost_weight = key.priority as f64 * markup` — purely static, no runtime spend (`crates/gw-routing/src/pipeline/mod.rs:283`).

There is therefore **no accumulated cost window to clear**. The only per-`(key, format)` *runtime sliding-window state that actually gates/influences routing* is `FormatHealth` in `gw-health` (`KeyHealthStore`), which is already wired into the command consumer via `CommandConsumer.health`.

Critically, the existing `clear-cooldown` only zeroes the cooldown deadline + reason — it does **not** reset a tripped circuit, the 30s failure sliding window, or `consecutive_failures` (`FormatHealth::clear_cooldown`, `format_health.rs:192`). So a key that auto-tripped its breaker (≥20 samples, >50% failure → `Open` for 60s) cannot be force-recovered by clear-cooldown. That is the gap `reset-cost` fills.

## Decision

`reset-cost` performs a **full runtime reset** of the per-`(key, format)` `FormatHealth` cell:

- empties the failure `SlidingWindow`,
- resets `consecutive_failures` to 0,
- forces the circuit back to `Closed` and clears `open_until_ms`,
- clears `cooldown_until_ms` + `cooldown_reason`.

This is real, demonstrable routing-runtime state (it flips a tripped/cooled key back to `is_available() == true`), distinct from `clear-cooldown`, and requires no new boot-time wiring (uses the already-injected `health`).

The wire action string is kept as **`reset-cost`** exactly as named in the Codex analysis, so the Rust ↔ Go contract agrees when Go later issues it through the same `GatewayCommandIssuer` path. Rust documents that, because it has no cost accumulator, the command resets the adaptive health/sliding-window runtime state backing routing.

### Args schema

Format-scoped, identical shape to `clear-cooldown` (so Go reuses the same arg builder):

```json
{ "key_id": "vk-…", "format": "openai|anthropic|gemini|kiro" }
```

`format` accepts the `api_format` alias (matching `CooldownArgs`/`ClearCooldownArgs`). Invalid/missing args → `failed` ACK with `invalid_args:…`; unknown format → `failed` ACK with `invalid_format`. Success → `ok` ACK `cost_runtime_reset` written **only after** the in-memory reset has run.

## Changes

1. `crates/gw-health/src/sliding_window.rs` — add `SlidingWindow::reset(&mut self)` (clears samples).
2. `crates/gw-health/src/format_health.rs` — add `FormatHealth::reset_runtime(&self)` (full reset described above).
3. `crates/gw-health/src/store.rs` — add `KeyHealthStore::reset_runtime(&self, key, fmt)` delegating to the cell; emit `gw_health_reset_runtime_total`.
4. `crates/gw-core/src/control_plane.rs` — add `GatewayCommandAction::ResetCost` (`as_str` → `"reset-cost"`); advertise `enabled_command(ResetCost)` in `default_gateway_command_capabilities` (placed after `ClearCache`, before the disabled block).
5. `crates/gw-gateway/src/control_plane/command_consumer.rs` — `ResetCostArgs { key_id, format(alias api_format) }`; `reset_cost` handler + free helper `reset_cost_outcome(&KeyHealthStore, &Value) -> CommandOutcome` (testable without Redis); dispatch arm `ResetCost => self.reset_cost(&entity.args)`.

## Tests (fail-before / pass-after)

- `gw-health`: `reset_runtime_clears_tripped_circuit_and_window` — trip the circuit + apply cooldown + accumulate consecutive failures, assert unavailable; `reset_runtime`; assert `is_available()` true, `consecutive_failures == 0`, circuit `Closed`, no cooldown. Contrast: `clear_cooldown` alone leaves circuit tripped.
- `command_consumer`: `reset_cost_outcome_resets_health_runtime` (valid args flip a tripped key available + `ok`/`cost_runtime_reset`); `reset_cost_outcome_rejects_bad_args` (`invalid_args:`); `reset_cost_args_accept_api_format_alias`; unknown format → `invalid_format`.
- `gw-core` wire-lock: add the `{"action":"reset-cost","enabled":true}` entry to `capability_snapshot_wire_paths_are_locked`.

## Doc sync (CLAUDE.md §Sync rule)

- `docs/go-contracts/parts/08-control-plane.md` — add `reset-cost` row to the §8.4.3 command table and the executable-hooks list in §As-built surface.
- `docs/spec/03-key-health.md` — document `KeyHealthStore::reset_runtime` and the `reset-cost` command as the admin full-reset path (vs `clear_cooldown`).

## Out of scope / non-goals

- No billing-ledger or wallet-settlement changes.
- No new boot-time plumbing (LatencyTracker is intentionally not touched: it is constructed after `control_plane::start` and is not the gating state).
- Go does not pretend `reset-cost` works until this lands.

## Review risk to flag

The action is named `reset-cost` but, because Rust computes no cost, it resets the adaptive **health/sliding-window** runtime state rather than a cost ledger. This is the only resettable routing-runtime state in Rust and is documented as such. If Codex/Go intends a different target, the wire string and handler body are the two touch-points to revisit.
