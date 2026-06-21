# Provider Model Cost Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add provider-model cost pricing weights, make Rust `cost_first` and `multi_dim.cost` use them, complete Go strategy configuration, and expose structured strategy controls in the frontend.

**Architecture:** Go remains the configuration authority and projects provider-model cost weights into the Rust `vendor_keys` payload. Rust consumes the already-loaded `VendorKey` and `Pricing` entities on the routing hot path, with missing provider-model weights falling back to `1000`. The frontend exposes provider-model cost weight as an opaque relative value and exposes model-group strategy config through first-class controls, using a table for inline `multi_dim`.

**Tech Stack:** Rust 1.88, serde, gw-core/gw-routing/gw-store, Go 1.26, sqlc v1.31.1, goose migrations, Vue 3, TypeScript, Vitest, existing shadcn-style UI components.

## Global Constraints

- Provider-model `cost_weight` is an integer with default `1000`.
- Valid provider-model `cost_weight` range is `1..=1_000_000`.
- Lower `cost_weight` is cheaper and preferred by `cost_first`.
- `1000` is the neutral baseline.
- The UI must label the field `Cost pricing weight`, not cost price.
- The UI must not expose real provider cost, margin, or commercial terms.
- Rust treats a missing projected model cost weight as `1000`.
- Go owns validation and must not write invalid projected model cost weights.
- Preserve legacy model-group strategy `"priority"` as `"fixed_order"`.
- Preserve preset-style multi-dimensional strategy `{ "multi_dim": "balanced" }`.
- Inline `multi_dim` must be configured through a table in the frontend, not raw JSON.
- Keep Rust request routing local to loaded model group and vendor key entities.

---

## Scope: Do / Don't

### Do

- Add `provider_models.cost_weight` (integer column, default `1000`, range `1..=1_000_000`) and project it into `vendor_keys.payload.model_cost_weights` via the existing `vendor_keys` view, exactly mirroring how `model_mapping` is projected today.
- Add `VendorKey.model_cost_weights: HashMap<ModelName, u32>` and `VendorKeyRow.model_cost_weights` (with `#[serde(default)]`); deserialization MUST happen via `VendorKeyRow` in `gw-store`, never directly on `VendorKey` (it has no `Deserialize` derive).
- Make `CandidateFeatures.cost_weight = key.model_cost_weights.get(ctx.model).copied().unwrap_or(1_000) as f64 * markup`. Missing entries fall back to `1000`. `CostFirstPolicy.score` and `needed_dims` keep their current pricing-aware shape: `cost_weight` is used only when `ctx.pricing.is_some()`, and `key.priority` remains the fallback when pricing is absent.
- Replace `StrategyKind::MultiDim(String)` with `MultiDimPreset(String) + MultiDimInline(MultiDimWeights)`. Implement `Deserialize` for `StrategyKind` manually via a wire enum so the wire shapes `{ "multi_dim": "balanced" }` and `{ "multi_dim": { "health": 0.25, ... } }` both round-trip.
- Add `StrategyChoice::{Borrowed, Owned}` plus `StrategySet::strategy_for_kind`, and switch `gw-proxy::filters::upstream` to it. Owned variant is allocated only for inline `multi_dim`; all other variants stay borrowed.
- In Go, change `GroupUpsertInput.Strategy` from `*string` to `json.RawMessage`, change `payloadOverrides()` to return `(map[string]any, error)`, and update BOTH call sites (`Create` at `catalog.go:1229`, `Update` at `catalog.go:1456`).
- Replace the existing `normalizeGatewayModelGroupStrategy(string) string` (`catalog.go:57`) with the new `normalizeGatewayModelGroupStrategyValue(json.RawMessage) (any, error)` + `normalizeGatewayModelGroupStrategyLiteral(string) (string, error)` pair. Delete the old function and the now-stale `TestNormalizeGatewayModelGroupStrategyKeepsRustStrategies` test it covered.
- Validate provider-model `cost_weight` and model-group strategy in Go business layer; map both errors to HTTP `400` in `routes.go`.
- Add `cost_weight` to provider-model frontend types/form/mocks. Add a structured strategy selector (and a `multi_dim` weights table) to the model-group dialog; submit either a string literal or `{ multi_dim: <weights | preset> }` as a typed object.
- Update `scripts/check-migration-number.sh` and `scripts/verify-migrations-roundtrip.sh` to extend their hardcoded migration whitelists to include `00077`.

### Don't

- Don't add `Deserialize` to the public `gw_core::entity::VendorKey` struct — it is constructed manually from `VendorKeyRow` in `gw-store::loader::load_vendor_key`. Tests that need to verify the wire shape of cost weights MUST target `VendorKeyRow` (in a `#[cfg(test)]` block inside `crates/gw-store/src/loader.rs`, since `VendorKeyRow` is private to that crate) and use `api_key_enc: EncryptedKey`, NOT `api_key: SecretString`.
- Don't add or change `Serialize` on `StrategyKind`. The gateway only reads strategy from Go-projected JSON; nothing serializes it. Removing the existing `Serialize` derive is intentional and required so the wire enum is the single source of deserialization truth.
- Don't change `CostFirstPolicy.needed_dims` or `CostFirstPolicy.score`. They must keep using `features.cost_weight` only when `ctx.pricing.is_some()` and `key.priority` otherwise — the behavior change comes entirely from how `features.cost_weight` is computed in the pipeline.
- Don't break the preset wire shape `{ "multi_dim": "balanced" }`. Don't break the legacy `"priority"` literal (normalize it to `"fixed_order"`).
- Don't expose real provider cost, margin, vendor pricing, or commercial terms in the UI. The field is labelled **Cost pricing weight** and the help text states it is a relative weight, not a real cost.
- Don't expose model-group strategy as a free-form JSON editor in the UI. Strategy MUST be a typed select; `multi_dim` MUST be a typed weights table.
- Don't change the legacy `[routing.presets]` resolution path or remove the `gw_routing_unknown_preset_total` metric / `GW_ROUTING_UNKNOWN_PRESET` warn-once behavior for preset-style configs.
- Don't widen wire DTOs beyond `cost_weight` and `strategy`. In particular, don't add `is_available` to `modelCreateWire` (it's a separate omission — out of scope here).
- Don't auto-wire any new `events:entity ns=vendor_key` invalidation calls. `emitVendorKeyInvalidationsForProvider` is already invoked from `ModelService.Create`, `Update`, and `SoftDelete` (`catalog.go:809`, `:909`, `:926`); cost-weight changes flow through those paths and need no extra hooks.
- Don't try to derive a numeric Rust strategy enum for `cost_weight` — `cost_first` is the existing strategy, and "cost weight" here refers to the per-model weight column, not a new strategy variant. Don't add `"cost_weight"` to `rustModelGroupStrategies`.
- Don't change cross-process contracts beyond adding `model_cost_weights` to `vendor_keys.payload`. Don't add or rename Redis streams, counter keys, or other JSONB columns.
- Don't add new migrations between 00076 and 00077, and don't renumber existing migrations. The next file MUST be exactly `00077_provider_model_cost_weight.sql`.
- Don't run `cargo fmt --all -- --check` or `cargo clippy --workspace --all-targets -- -D warnings` as task-level gates inside the plan — they belong to CI. Within-task verification is the focused `cargo test -p <crate> ...` commands shown.

### Out of Scope (do NOT touch in this plan)

- Pre-existing `modelCreateWire` ↔ `ModelCreateInput` divergences (e.g. `is_available`).
- Frontend visualization or analytics of cost weight (dashboards, charts, exports).
- Gateway-side cost computation or billing — `no_cost_micro` / `no_billing_crate` lints remain authoritative.
- Tunnel, capability, ratelimit, KMS, codec, observability, and unrelated migration cleanup.
- Doc churn beyond the spec file directly modified by this work (no `docs/architecture.md`, `docs/workflow.md`, or `docs/spec/` changes are required by this plan — call them out as a follow-up if a touched behavior crosses the §Sync rule boundary).

---

## File Structure

- Modify `ailinkmax-gateway/crates/gw-core/src/entity.rs`
  - Add `VendorKey.model_cost_weights`.
  - Replace `StrategyKind::MultiDim(String)` with a shape that supports preset and inline weights.

- Modify `ailinkmax-gateway/crates/gw-store/src/loader.rs`
  - Deserialize `model_cost_weights` from vendor key payload.
  - Pass the field into `VendorKey`.

- Modify `ailinkmax-gateway/crates/gw-routing/src/pipeline/mod.rs`
  - Compute `CandidateFeatures.cost_weight` from provider-model weight and pricing markup.

- Modify `ailinkmax-gateway/crates/gw-routing/src/strategy/set.rs`
  - Dispatch inline `multi_dim` without requiring a boot-time preset.
  - Keep unknown preset fallback to fixed order.

- Modify Rust tests in `ailinkmax-gateway/crates/gw-core/tests`, `ailinkmax-gateway/crates/gw-routing/tests`, and strategy module tests.
  - Cover default vendor-key deserialization, provider-model cost routing, missing weight fallback, inline multi-dimensional dispatch, and preset compatibility.

- Create `AILinkMax-Go/migrations/00077_provider_model_cost_weight.sql`
  - Add `provider_models.cost_weight`.
  - Add a check constraint.
  - Project `model_cost_weights` into `vendor_keys.payload`.

- Modify `AILinkMax-Go/modules/model/model/queries/provider_models.sql`
  - Include `cost_weight` in create/update/select return paths.

- Regenerate `AILinkMax-Go/modules/model/model/sqlcgen/*.go`
  - Use `make sqlc`.

- Modify `AILinkMax-Go/modules/model/business/catalog.go`
  - Add cost-weight DTO fields.
  - Validate range on create/update.
  - Accept structured model-group strategy values.
  - Return normalized strategy from model-group payload.

- Modify `AILinkMax-Go/modules/model/api/routes_advanced.go`
  - Add `cost_weight` to provider-model wire DTO.

- Modify `AILinkMax-Go/modules/model/api/routes.go`
  - Return `400` for invalid provider-model cost weights or invalid strategy payloads.

- Modify Go tests in `AILinkMax-Go/modules/model/business` and `AILinkMax-Go/modules/model/api`.
  - Cover provider-model cost defaults, validation, strategy string and inline `multi_dim` contract, and DTO readback.

- Modify `AILinkMax-Go/frontend/src/api/endpoints/types/model.ts`
  - Add `cost_weight` to provider-model API types.

- Modify `AILinkMax-Go/frontend/src/api/model-groups.ts`
  - Add strategy union types and request/response fields.

- Modify `AILinkMax-Go/frontend/src/features/providers/components/ProviderModelFormDialog.vue`
  - Add `Cost pricing weight` numeric input.
  - Submit `cost_weight` on create/update.

- Modify `AILinkMax-Go/frontend/src/features/models/components/ModelGroupFormDialog.vue`
  - Add strategy selector.
  - Add inline `multi_dim` table.
  - Submit string or structured strategy payload.

- Modify `AILinkMax-Go/frontend/src/mocks/data.ts` and `AILinkMax-Go/frontend/src/mocks/handler.ts`
  - Keep mock provider models and model groups in sync with new fields.

---

### Task 1: Rust Vendor Key Cost Weight

**Files:**
- Modify: `ailinkmax-gateway/crates/gw-core/src/entity.rs`
- Modify: `ailinkmax-gateway/crates/gw-store/src/loader.rs` (entity field + `VendorKeyRow` field + an in-file `#[cfg(test)]` test for the row's deserialization)
- Modify: Rust files containing `VendorKey { ... }` literals reported by `rg -n "VendorKey \\{" crates tests`
- Test: `ailinkmax-gateway/crates/gw-core/tests/vendor_key_identity.rs` (struct-literal-only assertion)

**Interfaces:**
- Produces: `VendorKey.model_cost_weights: HashMap<ModelName, u32>`
- Consumes: `vendor_keys.payload.model_cost_weights` (via `VendorKeyRow` in `gw-store::loader`)
- Later tasks consume `VendorKey.model_cost_weights` from `gw-routing`.

**Important context (read before editing):**
- `gw_core::entity::VendorKey` is NOT `Deserialize`. It is constructed manually by `gw_store::loader::load_vendor_key` from a private `VendorKeyRow` struct that IS `Deserialize`. All serde tests for the new field belong inside `crates/gw-store/src/loader.rs` (`#[cfg(test)] mod tests` already exists at the bottom of the file and already deserializes `VendorKeyRow` — extend it).
- `VendorKeyRow` uses `api_key_enc: EncryptedKey`, NOT `api_key: SecretString`. Tests must follow the existing in-file test shape (see existing rows around `loader.rs:680`) — don't invent a new wire shape.

- [ ] **Step 1: Write the failing struct-literal test (gw-core)**

Add this test to `ailinkmax-gateway/crates/gw-core/tests/vendor_key_identity.rs` (this file currently has one `vendor_key_identity_snapshot_defaults_to_none` test; append below it):

```rust
#[test]
fn vendor_key_struct_literal_sets_model_cost_weights() {
    use gw_core::entity::{KeyStatus, VendorKey};
    use gw_core::ids::{KeyId, ModelName, VendorId};
    use secrecy::SecretString;
    use std::collections::HashMap;

    let mut weights = HashMap::new();
    weights.insert(ModelName::from("gpt-4o"), 900);

    let key = VendorKey {
        key_id: KeyId::from("vk_1"),
        vendor_id: VendorId::from("v_1"),
        api_key: SecretString::new("sk-test".into()),
        accessible_models: vec![ModelName::from("gpt-4o")],
        model_mapping: HashMap::new(),
        model_cost_weights: weights,
        accessible_endpoints: vec![],
        protocol_conversion: true,
        priority: 10,
        weight: 1,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: KeyStatus::Active,
        tags: vec![],
        forced_capabilities: vec![],
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,
        quota_state: Default::default(),
    };

    assert_eq!(
        key.model_cost_weights.get(&ModelName::from("gpt-4o")),
        Some(&900),
    );
}
```

- [ ] **Step 1b: Write the failing `VendorKeyRow` deserialization tests (gw-store)**

Inside `ailinkmax-gateway/crates/gw-store/src/loader.rs`, find the existing `#[cfg(test)] mod tests` block (it currently deserializes a `VendorKeyRow` around line 680). Add these two tests **inside** that module, mirroring the existing test's JSON shape (in particular: `api_key_enc` is an object, not a string — copy the exact shape used by the existing in-module test that exercises `VendorKeyRow`):

```rust
#[test]
fn vendor_key_row_model_cost_weights_default_to_empty() {
    // Copy the JSON shape from the existing VendorKeyRow test in this module
    // (the one near the bottom of loader.rs that already builds a VendorKeyRow).
    // Omit `model_cost_weights` from the JSON entirely so #[serde(default)] kicks in.
    let payload = existing_vendor_key_row_json_without_cost_weights();
    let row: VendorKeyRow = serde_json::from_value(payload).expect("row deserializes");
    assert!(row.model_cost_weights.is_empty());
}

#[test]
fn vendor_key_row_deserializes_model_cost_weights() {
    let mut payload = existing_vendor_key_row_json_without_cost_weights();
    payload["model_cost_weights"] = serde_json::json!({ "gpt-4o": 850 });
    let row: VendorKeyRow = serde_json::from_value(payload).expect("row deserializes");
    assert_eq!(
        row.model_cost_weights
            .get(&gw_core::ids::ModelName::from("gpt-4o")),
        Some(&850),
    );
}
```

`existing_vendor_key_row_json_without_cost_weights()` is a small test helper you add at the top of the `mod tests` block by extracting the JSON literal already present in the file's existing VendorKeyRow test. Do not invent fields; use the exact shape the working test uses (notably `api_key_enc` as an `{ "ciphertext": "...", "nonce": "...", "kek_id": "..." }`-shaped object — match the file's existing helper EXACTLY).

- [ ] **Step 2: Run the failing tests**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo test -p gw-core --test vendor_key_identity vendor_key_struct_literal_sets_model_cost_weights -- --nocapture
cargo test -p gw-store --lib vendor_key_row_model_cost_weights -- --nocapture
```

Expected: compile fails because neither `VendorKey` nor `VendorKeyRow` has a `model_cost_weights` field.

- [ ] **Step 3: Add the Rust entity field**

In `ailinkmax-gateway/crates/gw-core/src/entity.rs`, add this field to the `VendorKey` struct immediately after `model_mapping`:

```rust
    pub model_cost_weights: HashMap<ModelName, u32>,
```

Note: `VendorKey` has NO `#[derive(Deserialize)]`, so don't add a `#[serde(default)]` attribute here — the attribute is meaningless on a non-serde struct. The wire-shape `#[serde(default)]` goes on `VendorKeyRow` in the next step.

- [ ] **Step 4: Add loader row support**

In `ailinkmax-gateway/crates/gw-store/src/loader.rs`, add this field to `VendorKeyRow` immediately after `model_mapping`:

```rust
    #[serde(default)]
    model_cost_weights: std::collections::HashMap<ModelName, u32>,
```

In `load_vendor_key`, pass it into `VendorKey` immediately after `model_mapping: row.model_mapping,`:

```rust
            model_cost_weights: row.model_cost_weights,
```

- [ ] **Step 5: Fix `VendorKey` literals**

For every `VendorKey { ... }` literal reported by:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
rg -n "VendorKey \\{" crates tests
```

add this field immediately after `model_mapping: ...`:

```rust
        model_cost_weights: Default::default(),
```

When a test needs explicit costs, use:

```rust
        model_cost_weights: HashMap::from([(ModelName::from("gpt-4o"), 850)]),
```

- [ ] **Step 6: Run the Rust entity test**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo test -p gw-core --test vendor_key_identity vendor_key_struct_literal_sets_model_cost_weights -- --nocapture
cargo test -p gw-store --lib vendor_key_row -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
git add crates/gw-core/src/entity.rs crates/gw-core/tests/vendor_key_identity.rs crates/gw-store/src/loader.rs crates tests
git commit -m "feat: add vendor key model cost weights"
```

---

### Task 2: Rust Cost-Aware Routing And Inline Multi-Dim

**Files:**
- Modify: `ailinkmax-gateway/crates/gw-core/src/entity.rs`
- Modify: `ailinkmax-gateway/crates/gw-routing/src/pipeline/mod.rs`
- Modify: `ailinkmax-gateway/crates/gw-routing/src/strategy/set.rs`
- Modify: `ailinkmax-gateway/crates/gw-routing/tests/strategies.rs`
- Modify: `ailinkmax-gateway/crates/gw-routing/tests/strategy_set.rs`
- Test: `ailinkmax-gateway/crates/gw-routing/tests/strategies.rs`
- Test: `ailinkmax-gateway/crates/gw-routing/tests/strategy_set.rs`

**Interfaces:**
- Consumes: `VendorKey.model_cost_weights`
- Produces: `StrategyKind::MultiDimPreset(String)` and `StrategyKind::MultiDimInline(DimWeights)`
- Produces: provider-model cost score `model_cost_weight_or_1000 * pricing.model_markup`
- Preserves: preset-style JSON `{ "multi_dim": "balanced" }`
- Adds: inline JSON `{ "multi_dim": { "health": 0.25, "latency": 0.25, "cost": 0.25, "inflight": 0.15, "error_rate": 0.10, "reservation": 0.0 } }`

- [ ] **Step 1: Write failing routing tests**

In `ailinkmax-gateway/crates/gw-routing/tests/strategies.rs`, change `fake_key` to accept optional model costs:

```rust
fn fake_key_with_cost(id: &str, prio: i32, model: &str, cost_weight: Option<u32>) -> Arc<VendorKey> {
    let model_cost_weights = cost_weight
        .map(|weight| HashMap::from([(ModelName::from(model), weight)]))
        .unwrap_or_default();

    Arc::new(VendorKey {
        key_id: KeyId::from(id),
        vendor_id: VendorId::from("v1"),
        api_key: SecretString::from("sk".to_string()),
        accessible_models: vec![],
        model_mapping: Default::default(),
        model_cost_weights,
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority: prio,
        weight: 1,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: KeyStatus::Active,
        tags: vec![],
        forced_capabilities: vec![],
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,
        quota_state: Default::default(),
    })
}
```

Add these tests:

```rust
#[test]
fn cost_first_uses_provider_model_cost_weight_when_pricing_exists() {
    use gw_routing::strategy::cost_first::CostFirst;

    let model = ModelName::from("gpt-4o");
    let pricing = Pricing {
        model: model.clone(),
        input_price_micro_per_token: 1,
        output_price_micro_per_token: 1,
        max_output: 4096,
        model_markup: 1.0,
        connect_timeout_ms: None,
        read_timeout_ms: None,
        total_timeout_ms: None,
        stream_total_timeout_ms: None,
    };
    let keys = vec![
        fake_key_with_cost("expensive", 100, "gpt-4o", Some(1500)),
        fake_key_with_cost("cheap", 1, "gpt-4o", Some(700)),
    ];
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &model,
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
        pricing: Some(&pricing),
        latency_tracker: &lt,
        reservation_view: None,
        frequency_tracker: None,
    };

    let picked = CostFirst.pick(&keys, &h, &f, &ctx).unwrap();
    assert_eq!(picked.key_id.as_ref(), "cheap");
}

#[test]
fn cost_first_missing_provider_model_cost_weight_uses_neutral_1000() {
    use gw_routing::strategy::cost_first::CostFirst;

    let model = ModelName::from("gpt-4o");
    let pricing = Pricing {
        model: model.clone(),
        input_price_micro_per_token: 1,
        output_price_micro_per_token: 1,
        max_output: 4096,
        model_markup: 1.0,
        connect_timeout_ms: None,
        read_timeout_ms: None,
        total_timeout_ms: None,
        stream_total_timeout_ms: None,
    };
    let keys = vec![
        fake_key_with_cost("missing", 100, "other-model", Some(1)),
        fake_key_with_cost("explicit-cheap", 1, "gpt-4o", Some(900)),
    ];
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &model,
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
        pricing: Some(&pricing),
        latency_tracker: &lt,
        reservation_view: None,
        frequency_tracker: None,
    };

    let picked = CostFirst.pick(&keys, &h, &f, &ctx).unwrap();
    assert_eq!(picked.key_id.as_ref(), "explicit-cheap");
}
```

- [ ] **Step 2: Write failing inline `multi_dim` tests**

In `ailinkmax-gateway/crates/gw-routing/tests/strategy_set.rs`, update `expected_name` to match the new enum variants:

```rust
        StrategyKind::MultiDimPreset(_) | StrategyKind::MultiDimInline(_) => "multi_dim",
```

Add these tests:

```rust
#[test]
fn strategy_set_dispatches_inline_multi_dim() {
    let set = StrategySet::new_default();
    let strategy_choice = set.strategy_for_kind(&StrategyKind::MultiDimInline(
        gw_core::entity::MultiDimWeights {
            health: 0.25,
            latency: 0.25,
            cost: 0.25,
            inflight: 0.15,
            error_rate: 0.10,
            reservation: 0.0,
        },
    ));
    let strategy = strategy_choice.as_strategy();
    assert_eq!(strategy.name(), "multi_dim");
}

#[test]
fn strategy_kind_deserializes_multi_dim_preset_and_inline_forms() {
    let preset: StrategyKind =
        serde_json::from_value(serde_json::json!({"multi_dim": "balanced"})).unwrap();
    assert!(matches!(preset, StrategyKind::MultiDimPreset(name) if name == "balanced"));

    let inline: StrategyKind = serde_json::from_value(serde_json::json!({
        "multi_dim": {
            "health": 0.25,
            "latency": 0.25,
            "cost": 0.25,
            "inflight": 0.15,
            "error_rate": 0.10,
            "reservation": 0.0
        }
    })).unwrap();
    assert!(matches!(inline, StrategyKind::MultiDimInline(_)));
}
```

- [ ] **Step 3: Run failing Rust routing tests**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo test -p gw-routing --test strategies cost_first -- --nocapture
cargo test -p gw-routing --test strategy_set multi_dim -- --nocapture
```

Expected: compile or assertion failure until the implementation is added.

- [ ] **Step 4: Add inline strategy entity shape**

In `ailinkmax-gateway/crates/gw-core/src/entity.rs`, import `DimWeights` from `gw-routing` is not allowed because `gw-core` must not depend on `gw-routing`. Add a local serializable shape instead:

```rust
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct MultiDimWeights {
    pub health: f64,
    pub latency: f64,
    pub cost: f64,
    pub inflight: f64,
    pub error_rate: f64,
    #[serde(default)]
    pub reservation: f64,
}
```

Change the `StrategyKind` derive to `#[derive(Clone, Debug)]`, then replace
`StrategyKind::MultiDim(String)` with public variants:

```rust
    MultiDimPreset(String),
    MultiDimInline(MultiDimWeights),
```

Add serde wire helpers:

```rust
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StrategyKindWire {
    CacheAffinity,
    FixedOrder,
    RoundRobin,
    LoadBalance,
    CostFirst,
    LatencyFirst,
    WeightedRr,
    Sticky,
    MultiDim(MultiDimStrategy),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum MultiDimStrategy {
    Preset(String),
    Inline(MultiDimWeights),
}
```

Implement `Deserialize` for public `StrategyKind` exactly through the wire enum:

```rust
impl<'de> Deserialize<'de> for StrategyKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = StrategyKindWire::deserialize(deserializer)?;
        Ok(match wire {
            StrategyKindWire::CacheAffinity => StrategyKind::CacheAffinity,
            StrategyKindWire::FixedOrder => StrategyKind::FixedOrder,
            StrategyKindWire::RoundRobin => StrategyKind::RoundRobin,
            StrategyKindWire::LoadBalance => StrategyKind::LoadBalance,
            StrategyKindWire::CostFirst => StrategyKind::CostFirst,
            StrategyKindWire::LatencyFirst => StrategyKind::LatencyFirst,
            StrategyKindWire::WeightedRr => StrategyKind::WeightedRr,
            StrategyKindWire::Sticky => StrategyKind::Sticky,
            StrategyKindWire::MultiDim(MultiDimStrategy::Preset(name)) => {
                StrategyKind::MultiDimPreset(name)
            }
            StrategyKindWire::MultiDim(MultiDimStrategy::Inline(weights)) => {
                StrategyKind::MultiDimInline(weights)
            }
        })
    }
}
```

Do not derive `Serialize` for `StrategyKind` in this task. The gateway reads
model-group strategy from Go-projected JSON and does not need to write this
field back through serde.

- [ ] **Step 5: Compute effective cost weight**

In `ailinkmax-gateway/crates/gw-routing/src/pipeline/mod.rs`, replace:

```rust
    let markup = ctx.pricing.map(|p| p.model_markup as f64).unwrap_or(1.0);
```

and:

```rust
            cost_weight: key.priority as f64 * markup,
```

with:

```rust
    let markup = ctx.pricing.map(|p| p.model_markup as f64).unwrap_or(1.0);
```

and:

```rust
            cost_weight: key
                .model_cost_weights
                .get(ctx.model)
                .copied()
                .unwrap_or(1_000) as f64
                * markup,
```

Do not change `CostFirstPolicy.needed_dims`: it must still use provider-model cost only when `ctx.pricing.is_some()`, preserving fallback to priority when pricing is absent.

- [ ] **Step 6: Dispatch inline `multi_dim`**

In `ailinkmax-gateway/crates/gw-routing/src/strategy/set.rs`, add a helper:

```rust
fn dim_weights_from_core(weights: gw_core::entity::MultiDimWeights) -> crate::pipeline::multi_dim::DimWeights {
    crate::pipeline::multi_dim::DimWeights::raw_with_reservation(
        weights.health,
        weights.latency,
        weights.cost,
        weights.inflight,
        weights.error_rate,
        weights.reservation,
    )
}
```

Change `pick_for` so preset and inline are handled separately:

```rust
            StrategyKind::MultiDimPreset(preset) => self.multi_dim.get(preset).unwrap_or_else(|| {
                if UNKNOWN_PRESETS_WARNED.get(preset).is_none() {
                    UNKNOWN_PRESETS_WARNED.insert(preset.clone(), ());
                    tracing::warn!(
                        code = gw_core::error::codes::GW_ROUTING_UNKNOWN_PRESET,
                        preset = %preset,
                        "unknown multi_dim routing preset referenced by a model group; falling back to fixed_order. Add it to [routing.presets] on this pod."
                    );
                }
                metrics::counter!(
                    "gw_routing_unknown_preset_total",
                    "code" => gw_core::error::codes::GW_ROUTING_UNKNOWN_PRESET,
                )
                .increment(1);
                &self.fixed_order
            }),
```

Add a borrowed-or-owned wrapper in `strategy/set.rs`:

```rust
pub enum StrategyChoice<'a> {
    Borrowed(&'a StrategyAny),
    Owned(StrategyAny),
}

impl StrategyChoice<'_> {
    pub fn as_strategy(&self) -> &StrategyAny {
        match self {
            StrategyChoice::Borrowed(strategy) => strategy,
            StrategyChoice::Owned(strategy) => strategy,
        }
    }
}
```

Then add:

```rust
    pub fn strategy_for_kind(&self, kind: &StrategyKind) -> StrategyChoice<'_> {
        match kind {
            StrategyKind::MultiDimInline(weights) => StrategyChoice::Owned(
                StrategyAny::MultiDim(MultiDim::new(
                    "inline".to_string(),
                    dim_weights_from_core(*weights),
                )),
            ),
            _ => StrategyChoice::Borrowed(self.pick_for(kind)),
        }
    }
```

Then change the proxy dispatch in `ailinkmax-gateway/crates/gw-proxy/src/filters/upstream.rs` from:

```rust
    let strategy = strategies.pick_for(&group.value.strategy);
```

to:

```rust
    let strategy_choice = strategies.strategy_for_kind(&group.value.strategy);
    let strategy = strategy_choice.as_strategy();
```

This keeps existing borrowed strategy instances for stateful variants and allocates only for inline `multi_dim`.

- [ ] **Step 7: Run Rust routing tests**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo test -p gw-routing --test strategies cost_first -- --nocapture
cargo test -p gw-routing --test strategy_set multi_dim -- --nocapture
cargo test -p gw-proxy upstream --lib -- --nocapture
```

Expected: PASS.

- [ ] **Step 8: Commit**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
git add crates/gw-core/src/entity.rs crates/gw-routing/src/pipeline/mod.rs crates/gw-routing/src/strategy/set.rs crates/gw-routing/tests/strategies.rs crates/gw-routing/tests/strategy_set.rs crates/gw-proxy/src/filters/upstream.rs
git commit -m "feat: route by provider model cost weights"
```

---

### Task 3: Go Provider Model Cost Weight And Vendor-Key Projection

**Files:**
- Create: `AILinkMax-Go/migrations/00077_provider_model_cost_weight.sql`
- Modify: `AILinkMax-Go/modules/model/model/queries/provider_models.sql`
- Regenerate: `AILinkMax-Go/modules/model/model/sqlcgen/provider_models.sql.go`
- Regenerate: `AILinkMax-Go/modules/model/model/sqlcgen/models.go`
- Modify: `AILinkMax-Go/modules/model/business/catalog.go`
- Modify: `AILinkMax-Go/modules/model/api/routes_advanced.go`
- Modify: `AILinkMax-Go/modules/model/api/routes.go`
- Modify: `AILinkMax-Go/scripts/check-migration-number.sh` (extend hardcoded migration whitelist to include 00077)
- Modify: `AILinkMax-Go/scripts/verify-migrations-roundtrip.sh` (extend hardcoded migration whitelist to include 00077)
- Test: `AILinkMax-Go/modules/model/business/catalog_test.go`
- Test: `AILinkMax-Go/modules/model/api/routes_wire_test.go`

**Interfaces:**
- Produces DB column: `provider_models.cost_weight INTEGER NOT NULL DEFAULT 1000`
- Produces JSON: provider-model DTO field `cost_weight`
- Produces projection: `vendor_keys.payload.model_cost_weights`
- Emits existing `events:entity ns=vendor_key` through `emitVendorKeyInvalidationsForProvider`.

- [ ] **Step 1: Write failing business tests**

Add to `AILinkMax-Go/modules/model/business/catalog_test.go`:

```go
func TestProviderModelRowToDTOCostWeight(t *testing.T) {
	row := sqlcgen.ProviderModel{
		ID: "pm_1", ProviderID: "v_1", GlobalModelID: "gm_1",
		ProviderModelName: "gpt-4o", CostWeight: 850,
		IsActive: true, IsAvailable: true,
	}
	dto := providerModelRowToDTO(row)
	require.Equal(t, int32(850), dto.CostWeight)
}

func TestNormalizeProviderModelCostWeight(t *testing.T) {
	got, err := normalizeProviderModelCostWeight(nil, 1000)
	require.NoError(t, err)
	require.Equal(t, int32(1000), got)

	v := int32(1)
	got, err = normalizeProviderModelCostWeight(&v, 1000)
	require.NoError(t, err)
	require.Equal(t, int32(1), got)

	tooLow := int32(0)
	_, err = normalizeProviderModelCostWeight(&tooLow, 1000)
	require.ErrorIs(t, err, ErrInvalidCostWeight)

	tooHigh := int32(1_000_001)
	_, err = normalizeProviderModelCostWeight(&tooHigh, 1000)
	require.ErrorIs(t, err, ErrInvalidCostWeight)
}
```

Add to `AILinkMax-Go/modules/model/api/routes_wire_test.go`:

```go
func TestModelCreateWireDecodeCostWeight(t *testing.T) {
	payload := `{
		"global_model_id": "gm_1",
		"provider_model_name": "gpt-5.4-mini",
		"cost_weight": 875
	}`

	var wire modelCreateWire
	require.NoError(t, json.Unmarshal([]byte(payload), &wire))
	in := wire.toInput()

	require.NotNil(t, in.CostWeight)
	require.Equal(t, int32(875), *in.CostWeight)
}
```

- [ ] **Step 2: Run failing Go tests**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/model
go test ./business ./api -run 'TestProviderModelRowToDTOCostWeight|TestNormalizeProviderModelCostWeight|TestModelCreateWireDecodeCostWeight' -count=1
```

Expected: compile fails because `CostWeight` and `ErrInvalidCostWeight` do not exist.

- [ ] **Step 3: Add migration**

Create `AILinkMax-Go/migrations/00077_provider_model_cost_weight.sql`:

```sql
-- +goose Up
ALTER TABLE provider_models
  ADD COLUMN IF NOT EXISTS cost_weight INTEGER NOT NULL DEFAULT 1000;

UPDATE provider_models
   SET cost_weight = 1000
 WHERE cost_weight IS NULL;

ALTER TABLE provider_models
  DROP CONSTRAINT IF EXISTS provider_models_cost_weight_range;

ALTER TABLE provider_models
  ADD CONSTRAINT provider_models_cost_weight_range
  CHECK (cost_weight BETWEEN 1 AND 1000000);

-- +goose StatementBegin
CREATE OR REPLACE FUNCTION alm_vendor_key_model_cost_weights(p_provider_id TEXT)
RETURNS JSONB
LANGUAGE sql
STABLE
AS $$
    SELECT COALESCE(
        jsonb_object_agg(gm.name, pm.cost_weight ORDER BY gm.name),
        '{}'::jsonb
    )
    FROM provider_models pm
    JOIN global_models gm ON gm.id = pm.global_model_id AND gm.deleted_at IS NULL
    WHERE pm.provider_id = p_provider_id
      AND pm.deleted_at IS NULL
      AND pm.is_active = true
      AND pm.is_available = true;
$$;
-- +goose StatementEnd

CREATE OR REPLACE VIEW vendor_keys AS
SELECT
    k.id,
    jsonb_set(
        jsonb_build_object(
            'key_id', k.id,
            'vendor_id', k.provider_id,
            'api_key_enc', alm_enc_to_rust_jsonb(k.api_key_enc),
            'accessible_models', CASE WHEN jsonb_typeof(k.allowed_models) = 'array' THEN k.allowed_models ELSE '[]'::jsonb END,
            'model_mapping', alm_vendor_key_model_mapping(k.provider_id),
            'model_cost_weights', alm_vendor_key_model_cost_weights(k.provider_id),
            'accessible_endpoints', alm_protocol_kind_array(k.api_formats),
            'protocol_conversion', true,
            'priority', k.internal_priority,
            'weight', COALESCE(k.weight, 1),
            'rpm_limit', COALESCE(k.rpm_limit, 0),
            'tpm_limit', 0,
            'max_usage_count', NULL,
            'proxy', NULL,
            'ip_allowlist', NULL,
            'expires_at', NULL,
            'status', CASE WHEN k.is_active AND NOT k.oauth_invalid THEN k.status ELSE 'disabled' END,
            'tags', '[]'::jsonb,
            'forced_capabilities', CASE
                WHEN jsonb_typeof(k.capabilities) = 'array' THEN k.capabilities
                WHEN jsonb_typeof(k.capabilities) = 'object' THEN alm_jsonb_true_keys(k.capabilities)
                ELSE '[]'::jsonb
            END,
            'exclusive_capability_match', false,
            'tunnel_preference', NULL,
            'oauth', COALESCE(k.oauth_payload_override, CASE
                WHEN k.auth_type = 'oauth' AND k.oauth_refresh_token_enc IS NOT NULL THEN jsonb_build_object(
                    'access_token_enc', alm_enc_to_rust_jsonb(k.api_key_enc),
                    'refresh_token_enc', alm_enc_to_rust_jsonb(k.oauth_refresh_token_enc),
                    'access_expires_at', COALESCE(k.oauth_expires_at / 1000, 0),
                    'token_endpoint', alm_oauth_token_endpoint(p.provider_type),
                    'scope', '[]'::jsonb
                )
                ELSE NULL
            END)
        ),
        '{quota_state}',
        jsonb_build_object(
            'availability', k.quota_availability,
            'remaining', k.quota_remaining,
            'limit', k.quota_limit,
            'reset_at_unix_s', CASE WHEN k.quota_reset_at IS NULL THEN NULL ELSE EXTRACT(EPOCH FROM k.quota_reset_at)::BIGINT END,
            'observed_at_unix_s', CASE WHEN k.quota_observed_at IS NULL THEN NULL ELSE EXTRACT(EPOCH FROM k.quota_observed_at)::BIGINT END,
            'source', k.quota_source
        ),
        true
    ) AS payload,
    k.version,
    k.deleted_at,
    k.identity_snapshot,
    k.quota_availability,
    k.quota_remaining,
    k.quota_limit,
    k.quota_reset_at,
    k.quota_observed_at,
    k.quota_source,
    to_timestamp(k.updated_at / 1000.0) AS updated_at
FROM provider_keys k
JOIN providers p ON p.id = k.provider_id;

-- +goose Down
CREATE OR REPLACE VIEW vendor_keys AS
SELECT
    k.id,
    jsonb_set(
        jsonb_build_object(
            'key_id', k.id,
            'vendor_id', k.provider_id,
            'api_key_enc', alm_enc_to_rust_jsonb(k.api_key_enc),
            'accessible_models', CASE WHEN jsonb_typeof(k.allowed_models) = 'array' THEN k.allowed_models ELSE '[]'::jsonb END,
            'model_mapping', alm_vendor_key_model_mapping(k.provider_id),
            'accessible_endpoints', alm_protocol_kind_array(k.api_formats),
            'protocol_conversion', true,
            'priority', k.internal_priority,
            'weight', COALESCE(k.weight, 1),
            'rpm_limit', COALESCE(k.rpm_limit, 0),
            'tpm_limit', 0,
            'max_usage_count', NULL,
            'proxy', NULL,
            'ip_allowlist', NULL,
            'expires_at', NULL,
            'status', CASE WHEN k.is_active AND NOT k.oauth_invalid THEN k.status ELSE 'disabled' END,
            'tags', '[]'::jsonb,
            'forced_capabilities', CASE
                WHEN jsonb_typeof(k.capabilities) = 'array' THEN k.capabilities
                WHEN jsonb_typeof(k.capabilities) = 'object' THEN alm_jsonb_true_keys(k.capabilities)
                ELSE '[]'::jsonb
            END,
            'exclusive_capability_match', false,
            'tunnel_preference', NULL,
            'oauth', COALESCE(k.oauth_payload_override, CASE
                WHEN k.auth_type = 'oauth' AND k.oauth_refresh_token_enc IS NOT NULL THEN jsonb_build_object(
                    'access_token_enc', alm_enc_to_rust_jsonb(k.api_key_enc),
                    'refresh_token_enc', alm_enc_to_rust_jsonb(k.oauth_refresh_token_enc),
                    'access_expires_at', COALESCE(k.oauth_expires_at / 1000, 0),
                    'token_endpoint', alm_oauth_token_endpoint(p.provider_type),
                    'scope', '[]'::jsonb
                )
                ELSE NULL
            END)
        ),
        '{quota_state}',
        jsonb_build_object(
            'availability', k.quota_availability,
            'remaining', k.quota_remaining,
            'limit', k.quota_limit,
            'reset_at_unix_s', CASE WHEN k.quota_reset_at IS NULL THEN NULL ELSE EXTRACT(EPOCH FROM k.quota_reset_at)::bigint END,
            'observed_at_unix_s', CASE WHEN k.quota_observed_at IS NULL THEN NULL ELSE EXTRACT(EPOCH FROM k.quota_observed_at)::bigint END,
            'source', k.quota_source
        ),
        true
    ) AS payload,
    k.version,
    k.deleted_at,
    k.identity_snapshot,
    k.quota_availability,
    k.quota_remaining,
    k.quota_limit,
    k.quota_reset_at,
    k.quota_observed_at,
    k.quota_source,
    to_timestamp(k.updated_at / 1000.0) AS updated_at
FROM provider_keys k
JOIN providers p ON p.id = k.provider_id;

ALTER TABLE provider_models
  DROP CONSTRAINT IF EXISTS provider_models_cost_weight_range;

ALTER TABLE provider_models
  DROP COLUMN IF EXISTS cost_weight;

DROP FUNCTION IF EXISTS alm_vendor_key_model_cost_weights(TEXT);
```

When implementing, copy the full current Up-side `vendor_keys` view body from
`00060_vendor_keys_write_compat.sql` and add only
`'model_cost_weights', alm_vendor_key_model_cost_weights(k.provider_id),` inside
`jsonb_build_object`. Keep `k.deleted_at`, `identity_snapshot`, quota columns,
and `updated_at`; dropping those columns breaks Rust runtime writeback
compatibility.

- [ ] **Step 4: Update provider model SQL and regenerate**

In `AILinkMax-Go/modules/model/model/queries/provider_models.sql`, add `cost_weight` to `CreateProviderModel` columns and params immediately after `provider_model_mappings`:

```sql
  id, provider_id, global_model_id, provider_model_name, provider_model_mappings,
  cost_weight,
  price_per_request, tiered_pricing, supports_vision, supports_function_calling,
```

The final create parameter order must be:

```sql
) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,TRUE,TRUE,1,$15,$15) RETURNING *;
```

The final update assignment order must be:

```sql
  provider_model_name = $2, provider_model_mappings = $3, cost_weight = $4,
  price_per_request = $5, tiered_pricing = $6, supports_vision = $7,
  supports_function_calling = $8, supports_streaming = $9,
  supports_extended_thinking = $10, supports_image_generation = $11,
  config = $12, is_active = $13, is_available = $14,
  version = version + 1, updated_at = $15
```

Regenerate:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
make sqlc
```

Expected generated structs include:

```go
CostWeight int32 `db:"cost_weight"`
```

- [ ] **Step 5: Add Go business validation and DTO fields**

In `AILinkMax-Go/modules/model/business/catalog.go`, add near existing errors:

```go
var ErrInvalidCostWeight = errors.New("model: invalid cost weight")
```

Add constants and helper near strategy constants:

```go
const defaultProviderModelCostWeight int32 = 1000
const minProviderModelCostWeight int32 = 1
const maxProviderModelCostWeight int32 = 1_000_000

func normalizeProviderModelCostWeight(input *int32, current int32) (int32, error) {
	value := current
	if input != nil {
		value = *input
	}
	if value < minProviderModelCostWeight || value > maxProviderModelCostWeight {
		return 0, ErrInvalidCostWeight
	}
	return value, nil
}
```

Add to `ModelDTO`:

```go
	CostWeight               int32          `json:"cost_weight"`
```

Add to `ModelCreateInput`:

```go
	CostWeight               *int32
```

In `providerModelRowToDTO`, set:

```go
		CostWeight:               r.CostWeight,
```

In `Create`, compute:

```go
	costWeight, err := normalizeProviderModelCostWeight(in.CostWeight, defaultProviderModelCostWeight)
	if err != nil {
		return ModelDTO{}, err
	}
```

and pass `CostWeight: costWeight` to `CreateProviderModelParams`.

In `Update`, compute:

```go
	costWeight, err := normalizeProviderModelCostWeight(in.CostWeight, cur.CostWeight)
	if err != nil {
		return ModelDTO{}, err
	}
```

and pass it to `UpdateProviderModelParams`.

- [ ] **Step 6: Add wire DTO field and 400 mapping**

In `AILinkMax-Go/modules/model/api/routes_advanced.go`, add to `modelCreateWire`:

```go
	CostWeight               *int32                  `json:"cost_weight"`
```

Set it in `toInput`:

```go
		CostWeight:               m.CostWeight,
```

In `AILinkMax-Go/modules/model/api/routes.go`, map `ErrInvalidCostWeight` to 400 in create and update:

```go
			if errors.Is(err, business.ErrInvalidCostWeight) {
				writeErr(w, http.StatusBadRequest, "invalid_cost_weight")
				return
			}
```

- [ ] **Step 7: Run Go provider-model tests**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/model
go test ./business ./api -run 'TestProviderModelRowToDTOCostWeight|TestNormalizeProviderModelCostWeight|TestModelCreateWireDecodeCostWeight' -count=1
```

Expected: PASS.

- [ ] **Step 8: Run migration static verification**

Both `scripts/check-migration-number.sh` and `scripts/verify-migrations-roundtrip.sh` currently hardcode migration whitelists that stop at `00076` (and `00075` respectively). Extend each script to include `00077_provider_model_cost_weight.sql` before running them — the change is one or two lines in each script (a number range or an explicit filename list). Read the scripts first; they're short.

Then run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
./scripts/verify-migrations-roundtrip.sh --static
./scripts/check-migration-number.sh
```

Expected: PASS.

- [ ] **Step 9: Commit**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add migrations/00077_provider_model_cost_weight.sql modules/model/model/queries/provider_models.sql modules/model/model/sqlcgen modules/model/business/catalog.go modules/model/api/routes.go modules/model/api/routes_advanced.go modules/model/business/catalog_test.go modules/model/api/routes_wire_test.go scripts/check-migration-number.sh scripts/verify-migrations-roundtrip.sh
git commit -m "feat: add provider model cost weights"
```

---

### Task 4: Go Model Group Strategy Contract

**Files:**
- Modify: `AILinkMax-Go/modules/model/business/catalog.go`
- Modify: `AILinkMax-Go/modules/model/api/routes.go`
- Test: `AILinkMax-Go/modules/model/business/catalog_decode_test.go`
- Test: `AILinkMax-Go/modules/model/business/catalog_test.go`

**Interfaces:**
- Produces: `GroupUpsertInput.Strategy json.RawMessage`
- Accepts: `"cost_first"`
- Accepts: `{ "multi_dim": "balanced" }`
- Accepts: `{ "multi_dim": { "health": 0.25, "latency": 0.25, "cost": 0.25, "inflight": 0.15, "error_rate": 0.10, "reservation": 0.0 } }`
- Rejects: unknown string strategies, negative weights, non-finite weights, and all-zero inline `multi_dim`.
- Returns: `ModelGroupDetail.strategy`

- [ ] **Step 1: Write failing strategy tests**

In `AILinkMax-Go/modules/model/business/catalog_decode_test.go`:

- **Delete** the existing `TestNormalizeGatewayModelGroupStrategyKeepsRustStrategies` test entirely — the function it covers (`normalizeGatewayModelGroupStrategy` at `catalog.go:57`) is being replaced in Step 3.
- **Add** the tests below in its place.

```go
func TestGroupUpsertPayloadOverridesAcceptsStrategyLiteral(t *testing.T) {
	raw := json.RawMessage(`"cost_first"`)
	in := GroupUpsertInput{Strategy: raw}

	overrides, err := in.payloadOverrides()

	require.NoError(t, err)
	require.Equal(t, "cost_first", overrides["strategy"])
}

func TestGroupUpsertPayloadOverridesNormalizesLegacyPriorityStrategy(t *testing.T) {
	raw := json.RawMessage(`"priority"`)
	in := GroupUpsertInput{Strategy: raw}

	overrides, err := in.payloadOverrides()

	require.NoError(t, err)
	require.Equal(t, "fixed_order", overrides["strategy"])
}

func TestGroupUpsertPayloadOverridesAcceptsInlineMultiDim(t *testing.T) {
	raw := json.RawMessage(`{
		"multi_dim": {
			"health": 0.25,
			"latency": 0.25,
			"cost": 0.25,
			"inflight": 0.15,
			"error_rate": 0.10,
			"reservation": 0.0
		}
	}`)
	in := GroupUpsertInput{Strategy: raw}

	overrides, err := in.payloadOverrides()

	require.NoError(t, err)
	require.Equal(t, map[string]any{
		"multi_dim": map[string]any{
			"health": float64(0.25),
			"latency": float64(0.25),
			"cost": float64(0.25),
			"inflight": float64(0.15),
			"error_rate": float64(0.10),
			"reservation": float64(0.0),
		},
	}, overrides["strategy"])
}

func TestGroupUpsertPayloadOverridesRejectsInvalidStrategy(t *testing.T) {
	raw := json.RawMessage(`"custom_future_strategy"`)
	in := GroupUpsertInput{Strategy: raw}

	_, err := in.payloadOverrides()

	require.ErrorIs(t, err, ErrInvalidStrategy)
}

func TestGroupUpsertPayloadOverridesRejectsInvalidMultiDim(t *testing.T) {
	for _, payload := range []string{
		`{"multi_dim":{"health":0,"latency":0,"cost":0,"inflight":0,"error_rate":0,"reservation":0}}`,
		`{"multi_dim":{"health":-1,"latency":0,"cost":0,"inflight":0,"error_rate":0,"reservation":0}}`,
	} {
		in := GroupUpsertInput{Strategy: json.RawMessage(payload)}
		_, err := in.payloadOverrides()
		require.ErrorIs(t, err, ErrInvalidStrategy)
	}
}
```

Add to `AILinkMax-Go/modules/model/business/catalog_test.go`:

```go
func TestModelGroupRowToDetailReturnsStrategy(t *testing.T) {
	g := sqlcgen.ModelGroup{
		ID: "mg_1", Name: "default", DisplayName: "Default",
		DefaultUserBillingMultiplier: 1.0, IsDefault: true, IsActive: true,
		Payload: []byte(`{"strategy":{"multi_dim":"balanced"}}`),
	}
	detail := modelGroupRowToDetail(g, nil, nil)
	require.Equal(t, map[string]any{"multi_dim": "balanced"}, detail.Strategy)
}
```

- [ ] **Step 2: Run failing Go strategy tests**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/model
go test ./business -run 'TestGroupUpsertPayloadOverrides|TestModelGroupRowToDetailReturnsStrategy' -count=1
```

Expected: compile failure because `Strategy` is a `*string`, `ErrInvalidStrategy` is missing, and `ModelGroupDetail` lacks `strategy`.

- [ ] **Step 3: Add strategy helpers**

In `AILinkMax-Go/modules/model/business/catalog.go`:

1. **Delete** the existing `normalizeGatewayModelGroupStrategy(strategy string) string` function (currently at `catalog.go:57`). The only caller is `payloadOverrides` at `catalog.go:1051`, which Step 4 rewrites.
2. **Add** the new helpers shown below.

Add the imports `math` and `strings` (verify both are imported; `strings` likely already is).

```go
var ErrInvalidStrategy = errors.New("model: invalid strategy")

type MultiDimWeights struct {
	Health      float64 `json:"health"`
	Latency     float64 `json:"latency"`
	Cost        float64 `json:"cost"`
	Inflight    float64 `json:"inflight"`
	ErrorRate   float64 `json:"error_rate"`
	Reservation float64 `json:"reservation"`
}

func normalizeGatewayModelGroupStrategyValue(raw json.RawMessage) (any, error) {
	trimmed := strings.TrimSpace(string(raw))
	if trimmed == "" || trimmed == "null" {
		return nil, nil
	}
	var literal string
	if err := json.Unmarshal(raw, &literal); err == nil {
		return normalizeGatewayModelGroupStrategyLiteral(literal)
	}
	var object map[string]json.RawMessage
	if err := json.Unmarshal(raw, &object); err != nil {
		return nil, ErrInvalidStrategy
	}
	multiDimRaw, ok := object["multi_dim"]
	if !ok || len(object) != 1 {
		return nil, ErrInvalidStrategy
	}
	var preset string
	if err := json.Unmarshal(multiDimRaw, &preset); err == nil {
		if strings.TrimSpace(preset) == "" {
			return nil, ErrInvalidStrategy
		}
		return map[string]any{"multi_dim": preset}, nil
	}
	var weights MultiDimWeights
	if err := json.Unmarshal(multiDimRaw, &weights); err != nil {
		return nil, ErrInvalidStrategy
	}
	if err := validateMultiDimWeights(weights); err != nil {
		return nil, err
	}
	return map[string]any{"multi_dim": map[string]any{
		"health": weights.Health, "latency": weights.Latency, "cost": weights.Cost,
		"inflight": weights.Inflight, "error_rate": weights.ErrorRate, "reservation": weights.Reservation,
	}}, nil
}

func normalizeGatewayModelGroupStrategyLiteral(strategy string) (string, error) {
	normalized := strings.TrimSpace(strings.ToLower(strategy))
	if normalized == legacyPriorityStrategy {
		return rustFixedOrderStrategy, nil
	}
	if _, ok := rustModelGroupStrategies[normalized]; ok && normalized != "multi_dim" {
		return normalized, nil
	}
	return "", ErrInvalidStrategy
}

func validateMultiDimWeights(weights MultiDimWeights) error {
	values := []float64{weights.Health, weights.Latency, weights.Cost, weights.Inflight, weights.ErrorRate, weights.Reservation}
	sum := 0.0
	for _, value := range values {
		if math.IsNaN(value) || math.IsInf(value, 0) || value < 0 {
			return ErrInvalidStrategy
		}
		sum += value
	}
	if sum <= 0 {
		return ErrInvalidStrategy
	}
	return nil
}
```

Add `math` to imports.

The plan-removed `normalizeGatewayModelGroupStrategy(string) string` function is now gone; if any leftover call site still references it, the build will fail — the only known caller is `payloadOverrides`, which Step 4 rewrites to call `normalizeGatewayModelGroupStrategyValue` instead.

- [ ] **Step 4: Change `GroupUpsertInput` and payload overrides**

Change:

```go
	Strategy            *string        `json:"strategy"`
```

to:

```go
	Strategy            json.RawMessage `json:"strategy"`
```

Change `payloadOverrides` signature:

```go
func (in GroupUpsertInput) payloadOverrides() (map[string]any, error) {
```

Change the strategy block:

```go
	if len(in.Strategy) > 0 {
		strategy, err := normalizeGatewayModelGroupStrategyValue(in.Strategy)
		if err != nil {
			return nil, err
		}
		if strategy != nil {
			out["strategy"] = strategy
		}
	}
	if len(out) == 0 {
		return nil, nil
	}
	return out, nil
```

Update `Create` and `Update` call sites:

```go
	overrides, err := in.payloadOverrides()
	if err != nil {
		return ModelGroupDetail{}, err
	}
	if err := s.mergeGroupPayload(ctx, tx, g.ID, overrides); err != nil {
		return ModelGroupDetail{}, err
	}
```

- [ ] **Step 5: Return strategy from model-group DTO**

Add to `ModelGroupDetail` in `catalog.go`:

```go
	Strategy any `json:"strategy,omitempty"`
```

Add helper:

```go
func strategyFromModelGroupPayload(payload []byte) any {
	if len(payload) == 0 {
		return "round_robin"
	}
	var obj map[string]any
	if err := json.Unmarshal(payload, &obj); err != nil {
		return "round_robin"
	}
	if strategy, ok := obj["strategy"]; ok {
		return strategy
	}
	return "round_robin"
}
```

Set in `modelGroupRowToDetail`:

```go
		Strategy: strategyFromModelGroupPayload(g.Payload),
```

- [ ] **Step 6: Map invalid strategy to 400**

In `AILinkMax-Go/modules/model/api/routes.go`, add to create/update group error handling:

```go
			if errors.Is(err, business.ErrInvalidStrategy) {
				writeErr(w, http.StatusBadRequest, "invalid_strategy")
				return
			}
```

- [ ] **Step 7: Run Go strategy tests**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/model
go test ./business ./api -run 'TestGroupUpsertPayloadOverrides|TestModelGroupRowToDetailReturnsStrategy' -count=1
```

Expected: PASS.

- [ ] **Step 8: Commit**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add modules/model/business/catalog.go modules/model/business/catalog_decode_test.go modules/model/business/catalog_test.go modules/model/api/routes.go
git commit -m "feat: complete model group routing strategy contract"
```

---

### Task 5: Frontend Provider Model Cost Weight

**Files:**
- Modify: `AILinkMax-Go/frontend/src/api/endpoints/types/model.ts`
- Modify: `AILinkMax-Go/frontend/src/features/providers/components/ProviderModelFormDialog.vue`
- Modify: `AILinkMax-Go/frontend/src/mocks/handler.ts`
- Test: `AILinkMax-Go/frontend/src/features/providers/components/ProviderModelFormDialog.vue`

**Interfaces:**
- Consumes and sends: `Model.cost_weight`, `ModelCreate.cost_weight`, `ModelUpdate.cost_weight`
- UI label: `Cost pricing weight`
- Help text: `Lower values are preferred by cost-first routing. This is a relative weight, not a real cost.`

- [ ] **Step 1: Add TypeScript fields**

In `AILinkMax-Go/frontend/src/api/endpoints/types/model.ts`, add to `Model`:

```ts
  cost_weight: number
```

Add to `ModelCreate`:

```ts
  cost_weight?: number
```

Add to `ModelUpdate`:

```ts
  cost_weight?: number
```

- [ ] **Step 2: Add the provider model form state**

In `ProviderModelFormDialog.vue`, add to `form`:

```ts
  cost_weight: 1000,
```

Set edit mode value:

```ts
        cost_weight: props.editingModel.cost_weight ?? 1000,
```

Set reset value:

```ts
    cost_weight: 1000,
```

- [ ] **Step 3: Add the form control**

Under the provider-model pricing section, add:

```vue
        <div class="flex items-center gap-3 pt-2 border-t">
          <Label class="text-xs whitespace-nowrap">Cost pricing weight</Label>
          <Input
            :model-value="form.cost_weight"
            type="number"
            step="1"
            min="1"
            class="w-32"
            @update:model-value="(v) => form.cost_weight = parseNumberInput(v, { min: 1 }) ?? 1000"
          />
          <span class="text-xs text-muted-foreground">
            Lower values are preferred by cost-first routing. This is a relative weight, not a real cost.
          </span>
        </div>
```

- [ ] **Step 4: Submit the cost weight**

In update payload, add:

```ts
        cost_weight: form.value.cost_weight,
```

In create payload, add:

```ts
        cost_weight: form.value.cost_weight,
```

- [ ] **Step 5: Keep mocks in sync**

`generateMockModelsForProvider` lives in `AILinkMax-Go/frontend/src/mocks/handler.ts` (around line 4384), NOT in `data.ts`. Edit handler.ts only.

In `AILinkMax-Go/frontend/src/mocks/handler.ts`, add `cost_weight: 1000` to every generated provider model object inside `generateMockModelsForProvider`.

In the provider model create handler (around `handler.ts:5616`) and update handler (around `handler.ts:5628`), explicitly default missing values so the mock matches the real API contract:

```ts
    cost_weight: Number(body.cost_weight ?? 1000),
```

`mocks/data.ts` does not need changes for Task 5 — leave it untouched.

- [ ] **Step 6: Run frontend type check**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/frontend
npm run type-check
```

Expected: PASS.

- [ ] **Step 7: Commit**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add frontend/src/api/endpoints/types/model.ts frontend/src/features/providers/components/ProviderModelFormDialog.vue frontend/src/mocks/handler.ts
git commit -m "feat: expose provider model cost weight"
```

---

### Task 6: Frontend Model Group Strategy Selector And Multi-Dim Table

**Files:**
- Modify: `AILinkMax-Go/frontend/src/api/model-groups.ts`
- Modify: `AILinkMax-Go/frontend/src/features/models/components/ModelGroupFormDialog.vue`
- Modify: `AILinkMax-Go/frontend/src/mocks/data.ts`
- Modify: `AILinkMax-Go/frontend/src/mocks/handler.ts`
- Test: `AILinkMax-Go/frontend/src/features/models/components/ModelGroupFormDialog.vue`

**Interfaces:**
- Produces frontend strategy type:
  - string literals: `fixed_order`, `round_robin`, `load_balance`, `weighted_rr`, `cost_first`, `latency_first`, `cache_affinity`, `sticky`
  - object: `{ multi_dim: MultiDimWeights | string }`
- UI sends raw JSON only through typed request body, never through a raw JSON editor.
- Default inline weights: `health=0.25`, `latency=0.25`, `cost=0.25`, `inflight=0.15`, `error_rate=0.10`, `reservation=0.0`.

- [ ] **Step 1: Add frontend strategy types**

In `AILinkMax-Go/frontend/src/api/model-groups.ts`, add:

```ts
export type ModelGroupStrategyLiteral =
  | 'fixed_order'
  | 'round_robin'
  | 'load_balance'
  | 'weighted_rr'
  | 'cost_first'
  | 'latency_first'
  | 'cache_affinity'
  | 'sticky'

export interface MultiDimWeights {
  health: number
  latency: number
  cost: number
  inflight: number
  error_rate: number
  reservation: number
}

export type ModelGroupStrategy =
  | ModelGroupStrategyLiteral
  | { multi_dim: string | MultiDimWeights }
```

Add to `ModelGroupSummary`:

```ts
  strategy?: ModelGroupStrategy
```

Add to `UpsertModelGroupRequest`:

```ts
  strategy?: ModelGroupStrategy
```

- [ ] **Step 2: Add model-group form strategy state**

In `ModelGroupFormDialog.vue`, add:

```ts
type StrategyMode = ModelGroupStrategyLiteral | 'multi_dim'

const DEFAULT_MULTI_DIM_WEIGHTS: MultiDimWeights = {
  health: 0.25,
  latency: 0.25,
  cost: 0.25,
  inflight: 0.15,
  error_rate: 0.10,
  reservation: 0.0,
}

const strategyOptions: Array<{ value: StrategyMode; label: string }> = [
  { value: 'fixed_order', label: 'Fixed order' },
  { value: 'round_robin', label: 'Round robin' },
  { value: 'load_balance', label: 'Load balance' },
  { value: 'weighted_rr', label: 'Weighted round robin' },
  { value: 'cost_first', label: 'Cost first' },
  { value: 'latency_first', label: 'Latency first' },
  { value: 'cache_affinity', label: 'Cache affinity' },
  { value: 'sticky', label: 'Sticky' },
  { value: 'multi_dim', label: 'Multi-dimensional' },
]

const multiDimRows = [
  { key: 'health', label: 'Health' },
  { key: 'latency', label: 'Latency' },
  { key: 'cost', label: 'Cost' },
  { key: 'inflight', label: 'In-flight' },
  { key: 'error_rate', label: 'Error rate' },
  { key: 'reservation', label: 'Reservation' },
] as const
```

Add to `form`:

```ts
  strategy_mode: 'round_robin' as StrategyMode,
  multi_dim_weights: { ...DEFAULT_MULTI_DIM_WEIGHTS },
```

- [ ] **Step 3: Parse existing strategy into form state**

Add helper:

```ts
function loadStrategy(strategy: ModelGroupStrategy | undefined) {
  if (!strategy) {
    form.value.strategy_mode = 'round_robin'
    form.value.multi_dim_weights = { ...DEFAULT_MULTI_DIM_WEIGHTS }
    return
  }
  if (typeof strategy === 'string') {
    form.value.strategy_mode = strategy
    form.value.multi_dim_weights = { ...DEFAULT_MULTI_DIM_WEIGHTS }
    return
  }
  form.value.strategy_mode = 'multi_dim'
  const value = strategy.multi_dim
  form.value.multi_dim_weights = typeof value === 'string'
    ? { ...DEFAULT_MULTI_DIM_WEIGHTS }
    : { ...DEFAULT_MULTI_DIM_WEIGHTS, ...value }
}

function buildStrategyPayload(): ModelGroupStrategy {
  if (form.value.strategy_mode !== 'multi_dim') {
    return form.value.strategy_mode
  }
  return {
    multi_dim: {
      health: form.value.multi_dim_weights.health,
      latency: form.value.multi_dim_weights.latency,
      cost: form.value.multi_dim_weights.cost,
      inflight: form.value.multi_dim_weights.inflight,
      error_rate: form.value.multi_dim_weights.error_rate,
      reservation: form.value.multi_dim_weights.reservation,
    },
  }
}
```

Call `loadStrategy(props.group.strategy)` in `loadGroupData`. Reset strategy to round-robin in `resetForm`.

- [ ] **Step 4: Add selector and table UI**

In the right-side model-group form column, above channel routes, add:

```vue
          <div class="space-y-3">
            <div class="flex items-center gap-2 border-b border-border/60 pb-2">
              <span class="text-sm font-medium">路由策略</span>
            </div>
            <div class="space-y-2">
              <Label class="text-xs font-medium">Strategy</Label>
              <Select v-model="form.strategy_mode">
                <SelectTrigger class="h-9">
                  <SelectValue placeholder="选择路由策略" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem
                    v-for="option in strategyOptions"
                    :key="option.value"
                    :value="option.value"
                  >
                    {{ option.label }}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div
              v-if="form.strategy_mode === 'multi_dim'"
              class="overflow-hidden rounded-lg border border-border"
            >
              <div class="grid grid-cols-[1fr_120px] bg-muted/50 px-3 py-2 text-xs font-medium text-muted-foreground">
                <span>Dimension</span>
                <span>Weight</span>
              </div>
              <div class="divide-y divide-border">
                <div
                  v-for="row in multiDimRows"
                  :key="row.key"
                  class="grid grid-cols-[1fr_120px] items-center gap-3 px-3 py-2"
                >
                  <span class="text-sm text-foreground">{{ row.label }}</span>
                  <Input
                    :model-value="form.multi_dim_weights[row.key]"
                    type="number"
                    min="0"
                    step="0.01"
                    class="h-8"
                    @update:model-value="(v) => form.multi_dim_weights[row.key] = parseNumberInput(v, { allowFloat: true, min: 0 }) ?? 0"
                  />
                </div>
              </div>
            </div>
          </div>
```

- [ ] **Step 5: Validate nonzero multi_dim weights**

Add computed:

```ts
const strategyError = computed(() => {
  if (form.value.strategy_mode !== 'multi_dim') return ''
  const total = Object.values(form.value.multi_dim_weights).reduce((sum, value) => sum + Number(value || 0), 0)
  return total > 0 ? '' : 'Multi-dimensional weights must include at least one positive value'
})
```

Include it in `isFormValid`:

```ts
const isFormValid = computed(() => !nameError.value && !displayNameError.value && !routeError.value && !strategyError.value)
```

- [ ] **Step 6: Submit strategy**

In `handleSubmit`, add:

```ts
      strategy: buildStrategyPayload(),
```

- [ ] **Step 7: Update mock model groups**

`MOCK_MODEL_GROUPS` lives in `AILinkMax-Go/frontend/src/mocks/data.ts` (referenced from handler.ts). Add `strategy: 'round_robin'` to each default mock group, and add one mock group that exercises the inline `multi_dim` shape:

```ts
    strategy: {
      multi_dim: {
        health: 0.25,
        latency: 0.25,
        cost: 0.25,
        inflight: 0.15,
        error_rate: 0.10,
        reservation: 0.0,
      },
    },
```

In `AILinkMax-Go/frontend/src/mocks/handler.ts`, in the model group create handler (around `handler.ts:3464`) and update handler (around `handler.ts:5748`), persist `body.strategy ?? 'round_robin'` onto the returned object so the mock round-trips the new field.

- [ ] **Step 8: Run frontend checks**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/frontend
npm run type-check
npm run test:run -- src/perf-overfetch-guard.test.ts
```

Expected: PASS.

- [ ] **Step 9: Commit**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add frontend/src/api/model-groups.ts frontend/src/features/models/components/ModelGroupFormDialog.vue frontend/src/mocks/data.ts frontend/src/mocks/handler.ts
git commit -m "feat: expose model group routing strategy controls"
```

---

### Task 7: Cross-Repo Verification

**Files:**
- Read: `ailinkmax-gateway/git status`
- Read: `AILinkMax-Go/git status`
- No source edits unless verification finds a concrete failure.

**Interfaces:**
- Verifies Rust routing, Go model contracts, frontend types, and migration static checks.

- [ ] **Step 1: Run Rust focused verification**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo test -p gw-core vendor_key_model_cost_weights -- --nocapture
cargo test -p gw-routing cost_first -- --nocapture
cargo test -p gw-routing multi_dim -- --nocapture
```

Expected: PASS.

- [ ] **Step 2: Run Go focused verification**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/model
go test ./business ./api -run 'TestProviderModel|TestNormalizeProviderModelCostWeight|TestModelCreateWireDecodeCostWeight|TestGroupUpsertPayloadOverrides|TestModelGroupRowToDetailReturnsStrategy' -count=1
```

Expected: PASS.

- [ ] **Step 3: Run migration verification**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
./scripts/check-migration-number.sh
./scripts/verify-migrations-roundtrip.sh --static
```

Expected: PASS.

- [ ] **Step 4: Run frontend verification**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/frontend
npm run type-check
npm run test:run -- src/perf-overfetch-guard.test.ts
```

Expected: PASS.

- [ ] **Step 5: Inspect worktrees**

Run:

```bash
git -C /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway status --short
git -C /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go status --short
```

Expected: only intended committed or staged changes remain. Unrelated user changes must not be reverted.

- [ ] **Step 6: Commit verification fixes**

When Step 5 shows tracked edits made during verification, commit them in the
repo where the fix landed:

```bash
git add <changed-files>
git commit -m "test: verify provider model cost routing"
```

Use the actual changed file list from `git status --short`.
