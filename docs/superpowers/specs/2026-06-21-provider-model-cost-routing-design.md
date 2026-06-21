# Provider Model Cost Weight and Routing Strategy Configuration Design

## Context

The Rust gateway already supports routing strategies on `ModelGroup.strategy`,
including `cost_first` and `multi_dim`. Today `cost_first` is not a true
provider cost strategy: Rust computes `cost_weight` from model-level pricing
markup, so two providers serving the same model cannot be ranked by their
relative upstream cost.

Go already owns provider, provider key, provider model, and model group
configuration. Frontend provider model forms already expose model pricing
surfaces, but true provider cost must not be shown directly to operators.
Instead, operators need an opaque cost ranking input that lets low-cost routes
win without disclosing the real cost basis.

Current `multi_dim` support is also incomplete for product use. Rust supports
named presets from boot-time `[routing.presets]`, while Go can only write the
string form cleanly. Frontend does not expose the full model-group strategy
surface.

## Goals

- Add a provider-model-level `cost_weight` managed by Go and frontend.
- Keep real provider cost hidden; expose only an opaque cost pricing weight.
- Make `cost_first` prefer lower provider-model cost weights.
- Complete Go support for all Rust model-group routing strategies.
- Add model-group frontend strategy configuration.
- Configure `multi_dim` through a table of dimensions, not raw JSON.
- Keep Rust request routing hot path local to the already loaded model group
  and vendor key entities.

## Non-Goals

- Do not expose real cost price, margin, or provider commercial terms in the UI.
- Do not move billing authority from Go to Rust.
- Do not make Rust compute actual per-token charge or user-facing price.
- Do not add a runtime API for editing Rust `[routing.presets]`.
- Do not change failover semantics.

## Cost Weight Semantics

The new provider model field is `cost_weight`.

- Type: integer.
- Default: `1000`.
- Valid range: `1..=1_000_000`.
- Lower is cheaper and preferred by `cost_first`.
- `1000` is the neutral baseline.
- Missing projected values are treated as `1000` by Rust.
- Go owns validation and projection; invalid values must not be written.

Operators should see this as "Cost pricing weight", not as a price. Suggested
helper copy: "Lower values are preferred by cost-first routing. This is a
relative weight, not a real cost."

## Data Model

### Go Database

Add a nullable-safe physical column to `provider_models`:

```sql
ALTER TABLE provider_models
  ADD COLUMN IF NOT EXISTS cost_weight INTEGER NOT NULL DEFAULT 1000;
```

The database migration should backfill existing rows with `1000` and enforce
positive values with a check constraint.

### Go DTOs and Business Logic

Provider model create, update, list, and detail DTOs include:

```json
{
  "cost_weight": 1000
}
```

Create defaults to `1000` when omitted. Update preserves the current value when
the field is omitted and writes a new value when present. Values outside the
valid range return `400`.

### Gateway Projection

Go projects provider-model cost weights into the Rust vendor key entity payload:

```json
{
  "model_cost_weights": {
    "gpt-4.1": 850,
    "claude-sonnet-4": 1200
  }
}
```

The map key is the client-facing model name used by Rust routing. This follows
the existing vendor-key projection pattern used for model mapping, so Rust does
not need a new provider-model entity lookup during request routing.

When a provider model changes, Go must update the affected vendor key payload
projection and emit the existing `events:entity` invalidation for
`ns=vendor_key`.

## Rust Routing Changes

### Entity Shape

`VendorKey` gains:

```rust
#[serde(default)]
pub model_cost_weights: HashMap<ModelName, u32>
```

The field must default to an empty map so existing payloads continue to
deserialize. Go must only project positive integers. A malformed payload is a
configuration error, not a runtime pricing signal.

### Cost Feature Extraction

Routing feature extraction changes `cost_weight` from model-only markup to:

```text
effective_cost_weight =
  vendor_key.model_cost_weights[requested_model].unwrap_or(1000)
  * pricing.model_markup
```

If `pricing` is absent, `cost_first` keeps its current fallback to priority.
If `pricing` is present but a vendor key has no model cost weight for the
requested model, Rust uses `1000`.

`multi_dim` uses the same extracted cost feature for its `cost` dimension.

### Strategy Shape

Rust keeps all existing strategy literals:

- `fixed_order`
- `round_robin`
- `load_balance`
- `weighted_rr`
- `cost_first`
- `latency_first`
- `cache_affinity`
- `sticky`

For `multi_dim`, Rust adds an inline strategy shape in addition to the existing
preset reference:

```json
{ "multi_dim": { "health": 0.25, "latency": 0.25, "cost": 0.25,
  "inflight": 0.15, "error_rate": 0.10, "reservation": 0.0 } }
```

Preset references remain supported for compatibility:

```json
{ "multi_dim": "balanced" }
```

Inline `multi_dim` should be fully self-contained in the model group payload and
must not depend on boot-time `[routing.presets]`.

## Go Model Group Strategy Changes

Go model-group create and update accept the full strategy surface:

```json
"strategy": "cost_first"
```

or inline multi-dimensional config:

```json
"strategy": {
  "multi_dim": {
    "health": 0.25,
    "latency": 0.25,
    "cost": 0.25,
    "inflight": 0.15,
    "error_rate": 0.10,
    "reservation": 0.0
  }
}
```

Go validates:

- string strategies must be one of the Rust literals above;
- `multi_dim` weights must be finite and non-negative;
- at least one `multi_dim` dimension must be greater than zero.

The legacy `"priority"` alias still normalizes to `"fixed_order"`.

## Frontend Changes

### Provider Model Form

In the provider model configuration surface, add a numeric field:

- Label: `Cost pricing weight`.
- Default: `1000`.
- Minimum: `1`.
- Step: `1`.
- Help text: "Lower values are preferred by cost-first routing. This is a
  relative weight, not a real cost."

The form sends `cost_weight` on create and update. Existing provider models
display `1000` when the API omits the field.

### Model Group Strategy UI

The model group form adds a strategy selector with all Rust strategies.

For ordinary strategies, the form sends the string value. When `multi_dim` is
selected, the form shows an editable table:

| Dimension | Weight |
| --- | --- |
| Health | numeric input |
| Latency | numeric input |
| Cost | numeric input |
| In-flight | numeric input |
| Error rate | numeric input |
| Reservation | numeric input |

Defaults for a new inline `multi_dim` are the current balanced preset:

```text
health=0.25, latency=0.25, cost=0.25, inflight=0.15,
error_rate=0.10, reservation=0.0
```

The UI must not expose raw JSON editing for this field.

## Testing

### Rust

- `gw-core`: deserializes `VendorKey` with missing `model_cost_weights`.
- `gw-routing`: `cost_first` picks the lower provider-model cost weight when
  pricing exists.
- `gw-routing`: missing model cost weight falls back to `1000`.
- `gw-routing`: inline `multi_dim` strategy dispatches and scores using the
  inline weights.
- `gw-routing`: preset-style `{"multi_dim":"balanced"}` remains compatible.

### Go

- Migration test or SQL regression for `provider_models.cost_weight`.
- Provider model create/update preserves omitted `cost_weight` and validates
  invalid values.
- Provider model list/detail returns `cost_weight`.
- Vendor key projection includes `model_cost_weights`.
- Model group create/update accepts all strategy literals.
- Model group create/update accepts inline `multi_dim` and rejects invalid
  all-zero or negative weights.

### Frontend

- Provider model form initializes `cost_weight` to `1000`.
- Provider model form submits changed `cost_weight`.
- Model group form displays the full strategy set.
- Selecting `multi_dim` renders the dimension table and submits structured
  weights.
- Existing model groups without strategy details remain editable.

## Rollout and Compatibility

- Existing provider models behave as `cost_weight=1000`.
- Existing vendor key payloads deserialize in Rust because
  `model_cost_weights` defaults to empty.
- Existing model group strategy strings remain valid.
- Existing `multi_dim` preset references remain valid.
- `cost_first` becomes meaningful only after Go projects non-default provider
  model cost weights.

## Operational Notes

The cost weight is a routing hint, not billing authority. Go billing continues
to compute actual charges from its pricing provider and usage records. Rust only
uses the projected weight to rank candidate vendor keys for route selection.
