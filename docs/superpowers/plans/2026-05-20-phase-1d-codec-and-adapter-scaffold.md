# Phase 1d — Codec S1 v2 Scaffold + gw-adapter v2 Trait Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the remaining "framework / scaffold" deliverables in Phase 1 — delete `gw-routing::retry.rs` (A1) + the inert `IrConvert / IrStreamConvert / IrErrorConvert` derives (A9), upgrade `Mutations` with the per-field setters that codec S1 v2 needs, scaffold the empty `wire_convert_matrix!` proc-macro and the `WireAny{Request,Response,StreamChunk}` enums in `gw-codec`, and stand up a brand-new `gw-adapter` crate carrying the v2 `UpstreamAdapter` trait (4 hooks + `transport_profile`), `AdapterCtx`, `NoopAdapter`, `AdapterRegistry`, and an `UpstreamErrorAnalyzer` trait skeleton.

**Architecture:** Project is pre-production; breaking changes are allowed and *no* compatibility shims are kept (`feedback-breaking-changes-ok`). All scaffolding lands without touching the live runtime's IR path — the actual N² wire→wire codegen and the rename `gw-core::ir → gw-core::schema` (A10 strict form) belong to Phase 2 where the runtime can move off `RequestIR` atomically. This re-timing is registered as a Living amendment in the integration spec §6 (Bm2's pattern from Phase 1c). The new `gw-adapter` crate is business-layer (per integration spec §3 C-1) and is referenced by no execution crate yet — `gw-proxy::filters::upstream` keeps its hard-coded `x-api-key` injection until Phase 3 wires the registry through. Two new `§5.2` lints (`no_legacy_retry`, `no_patch_body`) come online; `no_ir_runtime_struct` stays PENDING.

**Tech Stack:** Rust 1.86, `proc_macro2 / syn / quote` (for `wire_convert_matrix!`), `async-trait` (for the trait), `dashmap` (registry), `secrecy` (api_key handling), `thiserror` (error enum). No new runtime dependencies on the gateway hot path.

---

## File Structure

**Crates touched (existing):**

- `crates/gw-routing/` — delete `src/retry.rs`, `tests/retry.rs`; remove `pub mod retry;` + `pub use retry::*` from `src/lib.rs`.
- `crates/gw-proxy/src/gateway.rs` — drop `retry_cfg: RetryConfig` field + `RetryConfig` import + constructor param.
- `crates/gw-gateway/src/lib.rs` — drop `RetryConfig` import + the `RetryConfig::default()` arg.
- `crates/gw-gateway/tests/e2e_full_flow.rs`, `crates/gw-proxy/tests/end_to_end_openai.rs` — same.
- `crates/gw-codec-macros/src/lib.rs` — delete the three `proc_macro_derive(IrConvert / IrStreamConvert / IrErrorConvert)` entries.
- `crates/gw-codec-macros/src/{ir_convert,ir_stream,ir_error}.rs` — delete.
- `crates/gw-codec-macros/src/lib.rs` — add `mod wire_convert_matrix;` + the new `#[proc_macro]` entry.
- `crates/gw-codec/src/{openai,anthropic,gemini}/wire.rs` — remove `use gw_codec_macros::IrConvert;` + `#[derive(..., IrConvert)]` from each request/response/streamchunk struct.
- `crates/gw-core/src/ir.rs` — add the five new `Mutations::set_*` methods + tests.
- `crates/gw-codec/src/lib.rs` — declare the new `pub mod wire;` namespace.
- `scripts/check-invariants.sh` — activate `no_legacy_retry` and `no_patch_body`.
- `Cargo.toml` (workspace root) — add `crates/gw-adapter` member + the new `async-trait` workspace dep entry (if not already present); add `crates/gw-codec-macros/src/wire_convert_matrix.rs`.
- `CLAUDE.md` — add `gw-adapter` row to the workspace dep diagram; expand "Things intentional" with the schema-rename re-timing note.
- `docs/superpowers/specs/2026-05-20-v2-integration-design.md` §6 — append the Living amendment.

**Crates created:**

- `crates/gw-adapter/Cargo.toml` — new member.
- `crates/gw-adapter/src/lib.rs` — module index.
- `crates/gw-adapter/src/trait_def.rs` — `UpstreamAdapter` trait + `TransportProfile` / `ChromeVersion`.
- `crates/gw-adapter/src/error.rs` — `AdapterError` enum.
- `crates/gw-adapter/src/ctx.rs` — `AdapterCtx`, `HeadersMut`, `UrlMut`, `HeadersAccessor`, `UrlAccessor`.
- `crates/gw-adapter/src/noop.rs` — `NoopAdapter`.
- `crates/gw-adapter/src/analyzer.rs` — `UpstreamErrorAnalyzer` trait + `DefaultErrorAnalyzer` skeleton.
- `crates/gw-adapter/src/registry.rs` — `AdapterRegistry`.
- `crates/gw-adapter/tests/noop.rs` — Noop x-api-key injection unit test.
- `crates/gw-adapter/tests/registry.rs` — Registry lookup + fallback tests.

**Files created in gw-codec:**

- `crates/gw-codec/src/wire/mod.rs` — namespace.
- `crates/gw-codec/src/wire/any.rs` — `WireAnyRequest`, `WireAnyResponse`, `WireAnyStreamChunk` enums.
- `crates/gw-codec/tests/wire_any.rs` — construction + variant-tag check.

**Files created in gw-codec-macros:**

- `crates/gw-codec-macros/src/wire_convert_matrix.rs` — scaffold proc-macro.
- `crates/gw-codec-macros/tests/wire_convert_matrix.rs` — accepts valid syntax / errors on malformed.

---

## Spec coverage

| Phase 1 deliverable (integration §4.1) | Task | Notes |
|---|---|---|
| **A1** delete `gw-routing::retry.rs` | Task 1 | + activate `no_legacy_retry` lint (Task 10) |
| **A11** drop `RetryConfig::mode` / `RetryMode` | Task 1 | dies with `retry.rs` |
| **A8** drop `UpstreamAdapter::patch_body` + `BodyPatch` | Tasks 6-9 | trivially satisfied (gw-adapter is *new* — patch_body never lands); + activate `no_patch_body` lint (Task 10) |
| **A9** delete `IrConvert / IrStreamConvert / IrErrorConvert` derives | Task 2 | inert derive macros + their derive-attr uses |
| **A10** rename `gw-core::ir → gw-core::schema` + `#[non_exhaustive]` strip | **deferred to Phase 2** | runtime depends on `RequestIR::default()` etc; the strict strip lands in Phase 2 alongside the wire→wire migration; Living amendment registered in Task 10 |
| codec S1 v2 `Mutations` upgrade | Task 3 | new `set_*` for `system_prompt / messages / thinking_config / response_format / stream` |
| codec S1 v2 `wire_convert_matrix!` 宏框架 | Task 4 | proc-macro parses `schema: ... wires: [...]` syntax, emits empty TokenStream — full codegen is Phase 2 |
| codec S1 v2 `WireAny` enum | Task 5 | `WireAnyRequest<'a>` / `WireAnyResponse<'a>` / `WireAnyStreamChunk<'a>` scaffold; runtime path stays on `AnyCodec` until Phase 2 |
| gw-adapter trait v2 + Noop + Registry | Tasks 6-9 | full A-T0 scope |
| Bm3 — register `gw-adapter` in CLAUDE.md layering | Task 10 | (`gw-health` already there since Phase 1c; this adds the second business-layer crate) |
| Bm6 — clean v1 residue in business-split-refactor.md | (out of scope — already done in Phase 1a `0fb1131`) | verified via grep in Task 10 |
| `no_legacy_retry` activation | Task 10 | |
| `no_patch_body` activation | Task 10 | |

---

## Build-green discipline

The workspace stays green at every commit boundary. Task 1 removes `retry_cfg` from `LlmGateway::new` + all 3 call sites in one atomic commit. Task 2 removes the derive attributes from the 9 wire structs in the same commit that deletes the macro source files. Task 5 introduces `WireAny*` as new types that nothing consumes yet — the runtime keeps using `AnyCodec`. Task 9 adds `gw-adapter` as a workspace member that no execution crate depends on yet — the gateway binary still hard-codes `x-api-key` injection in `filters::upstream`. Phase 3 will rewire the proxy through `AdapterRegistry`.

---

### Task 1: Delete `gw-routing::retry.rs` and all RetryConfig/RetryMode/RetryExecutor references (A1 + A11)

**Files:**
- Delete: `crates/gw-routing/src/retry.rs`
- Delete: `crates/gw-routing/tests/retry.rs`
- Modify: `crates/gw-routing/src/lib.rs`
- Modify: `crates/gw-proxy/src/gateway.rs:18,35,54,65`
- Modify: `crates/gw-gateway/src/lib.rs:48,146`
- Modify: `crates/gw-gateway/tests/e2e_full_flow.rs:38,132`
- Modify: `crates/gw-proxy/tests/end_to_end_openai.rs:30,72,77`

- [ ] **Step 1: Delete `crates/gw-routing/src/retry.rs`**

```bash
git rm crates/gw-routing/src/retry.rs
git rm crates/gw-routing/tests/retry.rs
```

- [ ] **Step 2: Remove `retry` from `gw-routing` module index**

Update `crates/gw-routing/src/lib.rs` to:

```rust
//! gw-routing: 候选 Key 过滤、选择策略（8 种）、in-flight 计数。
//!
//! 主要导出：
//! - [`InFlightStore`] / [`InFlightGuard`] / [`InFlightCounter`] —— per-key
//!   in-flight counter (extracted from the retired `breaker` module per
//!   K-A ADR-KA-8).
//! - [`Strategy`] / [`StrategyAny`] / [`PickCtx`] —— 8 种策略 (ADR-009),
//!   now taking `&KeyHealthStore` + `&InFlightStore` (the retired single-store
//!   breaker is split: health → `gw-health`, in-flight → this crate).
//!
//! Retry / failover lives outside this crate. The legacy `retry.rs` was deleted
//! in Phase 1d per integration spec §2.1 A1; Phase 4 R-E introduces a dedicated
//! `gw-routing::failover` module on top of the FailoverState state machine.

pub mod candidates;
pub mod in_flight;
pub mod strategy;

pub use in_flight::{InFlightCounter, InFlightGuard, InFlightStore};
pub use strategy::{PickCtx, Strategy, StrategyAny};
```

- [ ] **Step 3: Strip `retry_cfg` from `LlmGateway`**

Edit `crates/gw-proxy/src/gateway.rs`:

Remove the `use gw_routing::RetryConfig;` line (around line 18). In the `LlmGateway` struct, delete the `pub retry_cfg: RetryConfig,` field. In `LlmGateway::new`, remove the `retry_cfg: RetryConfig,` parameter and the `retry_cfg,` line in the struct literal. The signature shrinks to:

```rust
impl LlmGateway {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<EntityStore>,
        health: Arc<KeyHealthStore>,
        in_flight: Arc<InFlightStore>,
        ratelimit: Arc<RateLimitGuard>,
        ledger: Arc<Ledger>,
        counters: Arc<CounterStore>,
        global_rpm: u32,
        trust_xff: bool,
    ) -> Self {
        Self {
            store,
            health,
            in_flight,
            ratelimit,
            ledger,
            counters,
            global_rpm,
            trust_xff,
        }
    }
}
```

- [ ] **Step 4: Drop `RetryConfig` arg from `gw-gateway::lib::run_gateway` call site**

Edit `crates/gw-gateway/src/lib.rs`:

- Delete `use gw_routing::RetryConfig;` (line 48).
- Remove the `RetryConfig::default(),` line (~146) from the `LlmGateway::new(...)` call.

- [ ] **Step 5: Update the two integration tests**

Edit `crates/gw-gateway/tests/e2e_full_flow.rs` and `crates/gw-proxy/tests/end_to_end_openai.rs`:

- Delete the `use gw_routing::RetryConfig;` import (`e2e_full_flow.rs:38`, `end_to_end_openai.rs:30`).
- Remove `RetryConfig::default(),` from the `LlmGateway::new` call in `e2e_full_flow.rs:132`.
- In `end_to_end_openai.rs`, the test has its own helper signature: drop the `retry_cfg: RetryConfig,` parameter (line ~72) and remove `retry_cfg` from the forwarded call (line ~77).

- [ ] **Step 6: Run cargo check + tests**

```bash
cargo check --workspace
cargo test --workspace
```

Expected: green. The retry tests are gone; nothing else in the proxy hot path called `RetryExecutor::should_retry` (per `grep`), so deletion is side-effect-free.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "$(cat <<'EOF'
refactor(gw-routing): delete retry.rs (A1 + A11)

retry.rs was vestigial — RetryExecutor::should_retry was never called from
the proxy hot path (per CLAUDE.md "intentional, not bugs": retry executor
deferred). RetryConfig::default() was constructed and stored in
LlmGateway::retry_cfg but never read. Drop the file, the field, the constructor
param, and all three call sites (gw-gateway::lib, e2e_full_flow, end_to_end_openai).

RetryMode dies with the file (A11). Phase 4 R-E introduces a dedicated
gw-routing::failover module driven by the FailoverState state machine.

Integration spec §2.1 A1 + A11.
EOF
)"
```

---

### Task 2: Delete `IrConvert / IrStreamConvert / IrErrorConvert` derives (A9)

**Files:**
- Modify: `crates/gw-codec/src/openai/wire.rs`
- Modify: `crates/gw-codec/src/anthropic/wire.rs`
- Modify: `crates/gw-codec/src/gemini/wire.rs`
- Modify: `crates/gw-codec-macros/src/lib.rs`
- Delete: `crates/gw-codec-macros/src/ir_convert.rs`
- Delete: `crates/gw-codec-macros/src/ir_stream.rs`
- Delete: `crates/gw-codec-macros/src/ir_error.rs`

- [ ] **Step 1: Remove `IrConvert` derive uses from the three wire modules**

For each of `crates/gw-codec/src/openai/wire.rs`, `crates/gw-codec/src/anthropic/wire.rs`, `crates/gw-codec/src/gemini/wire.rs`:

- Delete the `use gw_codec_macros::IrConvert;` line.
- For every struct/enum carrying `IrConvert` in its `#[derive(...)]`, remove that derive token. Example diff for an `OpenAiRequest`-style struct:

```diff
-#[derive(Debug, Deserialize, Serialize, IrConvert)]
+#[derive(Debug, Deserialize, Serialize)]
 #[ir(target = "RequestIR", direction = "both")]
 pub struct OpenAiRequest<'a> { ... }
```

The `#[ir(...)]` field-level attributes can stay — they will be picked up by `wire_convert_matrix!` in Phase 2. The struct-level `#[ir(target = "...")]` may *also* stay; both are inert without a consuming derive.

Verify with: `grep -rn "IrConvert\|IrStreamConvert\|IrErrorConvert" crates/gw-codec/ crates/gw-codec-macros/` should show only doc-comment hits in `gw-codec-macros/src/` (which we delete next), nothing in `gw-codec/src/`.

- [ ] **Step 2: Delete the three macro source files**

```bash
git rm crates/gw-codec-macros/src/ir_convert.rs
git rm crates/gw-codec-macros/src/ir_stream.rs
git rm crates/gw-codec-macros/src/ir_error.rs
```

- [ ] **Step 3: Strip the three `proc_macro_derive` declarations**

Edit `crates/gw-codec-macros/src/lib.rs` to:

```rust
//! `gw-codec-macros` — proc-macros for the `gw-codec` crate.
//!
//! This crate hosts a single `proc_macro` invoked from `gw-codec`:
//! `wire_convert_matrix!`. The matrix macro replaces the three legacy
//! `#[derive(IrConvert / IrStreamConvert / IrErrorConvert)]` macros that
//! were inert v1 scaffolding; those were removed in Phase 1d per
//! integration spec §2.1 A9.
//!
//! `wire_convert_matrix!` itself is a *framework* scaffold in Phase 1d —
//! it parses its input syntax and emits an empty `TokenStream`. Phase 2
//! (codec S1 v2 N² wire→wire codegen, §S1-v2-2) fills in the real
//! generation.

use proc_macro::TokenStream;

mod detect;
mod wire_convert_matrix;

#[proc_macro_derive(DetectRequest, attributes(detect))]
pub fn derive_detect_request(input: TokenStream) -> TokenStream {
    detect::expand(input)
}

#[proc_macro]
pub fn wire_convert_matrix(input: TokenStream) -> TokenStream {
    wire_convert_matrix::expand(input)
}
```

(The `wire_convert_matrix` module body is added in Task 4. For this step, create a placeholder `crates/gw-codec-macros/src/wire_convert_matrix.rs` containing:

```rust
use proc_macro::TokenStream;

pub(crate) fn expand(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}
```

so that `cargo check --workspace` passes at the end of this task.)

- [ ] **Step 4: Run cargo check**

```bash
cargo check --workspace --all-targets
```

Expected: green. The wire structs lose the inert derives; nothing depended on the generated impls (they were empty).

- [ ] **Step 5: Run the full test suite**

```bash
cargo test --workspace
```

Expected: green. Codec roundtrip tests pass because handwritten `via.rs` impls were the actual conversion path.

- [ ] **Step 6: Commit**

```bash
git add -u
git add crates/gw-codec-macros/src/wire_convert_matrix.rs
git commit -m "$(cat <<'EOF'
refactor(gw-codec-macros): delete IrConvert/IrStreamConvert/IrErrorConvert derives (A9)

The three derives were inert v1 scaffolding (they parsed #[ir(...)] attributes
but emitted empty TokenStreams; the actual From/TryFrom impls were hand-written
in gw-codec/src/<protocol>/via.rs). Codec S1 v2 replaces them with a single
top-level wire_convert_matrix! macro (scaffold lands in Task 4; full codegen
in Phase 2).

Remove #[derive(IrConvert)] from all wire structs in openai/anthropic/gemini.
Delete ir_convert.rs / ir_stream.rs / ir_error.rs. Lib.rs now exports
DetectRequest derive + wire_convert_matrix! scaffold.

Integration spec §2.1 A9 / §S1-v2-2.
EOF
)"
```

---

### Task 3: Upgrade `Mutations` with five new per-field setters

**Files:**
- Modify: `crates/gw-core/src/ir.rs:225-260` (the `Mutations` impl block)
- Test: `crates/gw-core/src/ir.rs` (add `#[cfg(test)]` block at end)

The current `Mutations` impl has `set_max_tokens / set_temperature / set_metadata / set_tools`. Codec S1 v2 §ADR-S1-v2-8 + the adapter v2 spec (A-T0 patch_request) need bits for the mutations adapters and filters actually perform. The five additions are: `set_system_prompt` (claude_code injects system), `set_messages` (post-merge filter may rewrite), `set_thinking_config` (anthropic thinking budget), `set_response_format` (json schema enforcement), `set_stream` (forced stream mode).

- [ ] **Step 1: Write the failing tests**

Append to `crates/gw-core/src/ir.rs`:

```rust
#[cfg(test)]
mod mutations_bits_tests {
    use super::Mutations;

    #[test]
    fn empty_by_default() {
        let m = Mutations::default();
        assert!(m.is_empty());
        assert_eq!(m.fields, 0);
    }

    #[test]
    fn each_setter_uses_a_distinct_bit() {
        let mut bits = std::collections::HashSet::new();
        for (name, setter) in setters() {
            let mut m = Mutations::default();
            setter(&mut m);
            assert!(!m.is_empty(), "{name} did not flip any bit");
            assert!(
                m.fields.count_ones() == 1,
                "{name} flipped more than one bit"
            );
            assert!(
                bits.insert(m.fields),
                "{name} flipped a bit another setter already owns"
            );
        }
    }

    #[test]
    fn setters_are_idempotent() {
        let mut m = Mutations::default();
        m.set_max_tokens();
        m.set_max_tokens();
        assert_eq!(m.fields.count_ones(), 1);
    }

    fn setters() -> Vec<(&'static str, fn(&mut Mutations))> {
        vec![
            ("max_tokens",      Mutations::set_max_tokens),
            ("temperature",     Mutations::set_temperature),
            ("metadata",        Mutations::set_metadata),
            ("tools",           Mutations::set_tools),
            ("system_prompt",   Mutations::set_system_prompt),
            ("messages",        Mutations::set_messages),
            ("thinking_config", Mutations::set_thinking_config),
            ("response_format", Mutations::set_response_format),
            ("stream",          Mutations::set_stream),
        ]
    }
}
```

- [ ] **Step 2: Run tests, confirm failure**

```bash
cargo test -p gw-core mutations_bits_tests
```

Expected: compile error — `set_system_prompt` / `set_messages` / `set_thinking_config` / `set_response_format` / `set_stream` don't exist.

- [ ] **Step 3: Add the five setters**

In `crates/gw-core/src/ir.rs`, extend the existing `impl Mutations { ... }` block (after `set_tools`) with:

```rust
    pub fn set_system_prompt(&mut self) {
        self.fields |= 1 << 4;
    }
    pub fn set_messages(&mut self) {
        self.fields |= 1 << 5;
    }
    pub fn set_thinking_config(&mut self) {
        self.fields |= 1 << 6;
    }
    pub fn set_response_format(&mut self) {
        self.fields |= 1 << 7;
    }
    pub fn set_stream(&mut self) {
        self.fields |= 1 << 8;
    }
```

- [ ] **Step 4: Run tests, confirm pass**

```bash
cargo test -p gw-core mutations_bits_tests
```

Expected: 3 tests pass.

- [ ] **Step 5: Workspace sanity check**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green. The existing passthrough bench / tests reading `Mutations::is_empty()` still see `is_empty() == true` after `default()`; no behavioral regression.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "$(cat <<'EOF'
feat(gw-core): Mutations adds 5 new per-field setters

system_prompt / messages / thinking_config / response_format / stream — these
are the mutations codec S1 v2 §ADR-S1-v2-8 and adapter v2 patch_request must
record so same-protocol passthrough fast-path (Mutations::is_empty) stays
correct after Phase 2 wires them through.

Bit positions 4..=8 are reserved; 0..=3 (max_tokens / temperature / metadata
/ tools) are unchanged. Tests pin the contract that each setter owns a
distinct bit and is idempotent.

Integration spec §S1-v2-8.
EOF
)"
```

---

### Task 4: Scaffold `wire_convert_matrix!` proc-macro

**Files:**
- Modify: `crates/gw-codec-macros/src/wire_convert_matrix.rs` (placeholder created in Task 2)
- Test: `crates/gw-codec-macros/tests/wire_convert_matrix.rs` (create)
- Modify: `crates/gw-codec-macros/Cargo.toml` if extra deps are needed (none expected — `syn full` already enabled)

The scaffold parses input of the form

```rust
wire_convert_matrix! {
    schema: RequestSchema,
    wires: [
        path::to::OpenAiRequest,
        path::to::AnthropicRequest,
    ],
}
wire_convert_matrix! {
    schema: StreamChunkSchema,
    wires: [
        path::to::OpenAiStreamChunk,
    ],
    stream: true,
}
```

and emits **an empty `TokenStream`**. Phase 2 §S1-v2-2 fills in the actual N² conversion + trait derivation. Until then, the scaffold's job is (a) to be callable from `gw-codec` without unblocking Phase 2's parser work, and (b) to give a precise `compile_error!` when malformed.

- [ ] **Step 1: Write failing parser tests**

Create `crates/gw-codec-macros/tests/wire_convert_matrix.rs`:

```rust
//! Surface-level acceptance tests for the wire_convert_matrix! scaffold.
//!
//! Full codegen lands in Phase 2; for Phase 1d we only verify the macro
//! parses the documented surface and rejects malformed inputs with a useful
//! compile_error message.

// Compile-time check: macro is callable with both shapes.
gw_codec_macros::wire_convert_matrix! {
    schema: DemoSchema,
    wires: [
        path::to::OpenAiRequest,
        path::to::AnthropicRequest,
    ],
}

gw_codec_macros::wire_convert_matrix! {
    schema: DemoStreamSchema,
    wires: [
        path::to::OpenAiStreamChunk,
    ],
    stream: true,
}

#[test]
fn macro_emits_nothing_runtime_visible() {
    // The scaffold emits an empty TokenStream — there is no item to assert on
    // at runtime. This test exists so cargo test exercises the macro
    // expansions above as part of the suite.
}
```

- [ ] **Step 2: Run, confirm failure (parser missing)**

```bash
cargo test -p gw-codec-macros
```

Expected: the two invocations above either compile to nothing (placeholder from Task 2) or compile_error. We want this test to *succeed* when the scaffold is correct.

- [ ] **Step 3: Implement the parser**

Replace `crates/gw-codec-macros/src/wire_convert_matrix.rs` with:

```rust
//! Scaffold for the `wire_convert_matrix!` proc-macro (codec S1 v2 §ADR-S1-v2-2).
//!
//! Phase 1d: this parses the documented surface syntax and emits an empty
//! `TokenStream`. Phase 2 fills in the N² wire→wire conversions, semantic
//! trait derivation, and `WireAny` enum generation.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Ident, LitBool, Path, Token, braced, bracketed};

/// `wire_convert_matrix! { schema: <Ident>, wires: [<Path>, ...], stream: <bool>? }`
pub(crate) struct MatrixInput {
    pub schema: Ident,
    pub wires: Vec<Path>,
    pub stream: bool,
}

impl Parse for MatrixInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut schema: Option<Ident> = None;
        let mut wires: Option<Vec<Path>> = None;
        let mut stream = false;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "schema" => {
                    let id: Ident = input.parse()?;
                    schema = Some(id);
                }
                "wires" => {
                    let content;
                    bracketed!(content in input);
                    let paths: Punctuated<Path, Token![,]> =
                        content.parse_terminated(Path::parse, Token![,])?;
                    wires = Some(paths.into_iter().collect());
                }
                "stream" => {
                    let v: LitBool = input.parse()?;
                    stream = v.value;
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown wire_convert_matrix! key `{other}` — expected one of \
                             `schema`, `wires`, `stream`"
                        ),
                    ));
                }
            }
            // Optional trailing comma between keys.
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        let schema = schema.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "wire_convert_matrix! requires a `schema: <Ident>` key",
            )
        })?;
        let wires = wires.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "wire_convert_matrix! requires a `wires: [<Path>, ...]` key",
            )
        })?;
        Ok(Self { schema, wires, stream })
    }
}

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let parsed = syn::parse_macro_input!(input as MatrixInput);
    // Phase 1d: no codegen — the parsed schema / wires / stream values are
    // consumed by Phase 2 §ADR-S1-v2-2 which generates the N² wire→wire
    // conversions, semantic traits, and WireAny enum.
    //
    // We touch each parsed field once so it is exercised (rather than
    // silently dead) when Phase 2 fills in the body.
    let _ = (&parsed.schema, &parsed.wires, parsed.stream);
    let empty = TokenStream2::new();
    quote!(#empty).into()
}
```

Then in `crates/gw-codec-macros/src/lib.rs`, also accept a wrapping `braced!` (the macro is most readable with `{ ... }`). Update the public entry:

```rust
#[proc_macro]
pub fn wire_convert_matrix(input: TokenStream) -> TokenStream {
    wire_convert_matrix::expand(input)
}
```

and update the `Parse` impl above to optionally accept `{ ... }` wrap — extend `MatrixInput::parse` first block:

```rust
impl Parse for MatrixInput {
    fn parse(outer: ParseStream) -> syn::Result<Self> {
        let input = if outer.peek(syn::token::Brace) {
            let content;
            braced!(content in outer);
            content
        } else {
            outer.fork()
        };
        // ... existing body using `input` ...
    }
}
```

Note: the brace-fork pattern matches the user invocations using `wire_convert_matrix! { ... }`. If you use the fork branch you must still parse from `outer` afterwards — simpler is to assign to a local `ParseBuffer`-equivalent. The cleanest implementation is to require the braces:

```rust
impl Parse for MatrixInput {
    fn parse(outer: ParseStream) -> syn::Result<Self> {
        let content;
        braced!(content in outer);
        // ... parse keys from `content` ...
    }
}
```

Pick this form. Update the demo invocations in the test if needed (they already use braces).

- [ ] **Step 4: Run tests**

```bash
cargo test -p gw-codec-macros
```

Expected: green. The two `wire_convert_matrix! { ... }` invocations in the test file parse + emit nothing; the `#[test]` runs and passes.

- [ ] **Step 5: Write a negative test (rejection)**

Append a `compile_fail` test or — simpler — add a comment block at the bottom of the test file demonstrating what *would* fail and which we may upgrade to `trybuild` in Phase 2. For Phase 1d, leave a one-line note:

```rust
// NOTE(phase-2): when codegen lands, switch to a trybuild fixture under
// tests/ui/ that verifies these specific compile_error messages:
//   - missing `schema:` key
//   - missing `wires:` key
//   - unknown key (e.g. `wirees:`)
//   - non-Ident schema value
```

- [ ] **Step 6: Workspace sanity check**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green.

- [ ] **Step 7: Commit**

```bash
git add crates/gw-codec-macros/
git commit -m "$(cat <<'EOF'
feat(gw-codec-macros): scaffold wire_convert_matrix! proc-macro

Codec S1 v2 §ADR-S1-v2-2 replaces the three legacy IrConvert derives with a
single top-level wire_convert_matrix! { schema, wires, stream } proc-macro.
Phase 1d lands the parser skeleton (accepts the documented surface syntax,
errors on unknown keys with a span-anchored message). Phase 2 fills in the
N² wire→wire codegen, semantic trait derivation, and WireAny enum generation.

Tests exercise both request-shape and stream-shape invocations; trybuild ui
fixtures arrive in Phase 2 to lock the failure messages.
EOF
)"
```

---

### Task 5: Scaffold `WireAny{Request,Response,StreamChunk}` enums

**Files:**
- Create: `crates/gw-codec/src/wire/mod.rs`
- Create: `crates/gw-codec/src/wire/any.rs`
- Modify: `crates/gw-codec/src/lib.rs` (add `pub mod wire;`)
- Test: `crates/gw-codec/tests/wire_any.rs` (create)

The current `gw-codec::AnyCodec` (in `src/any.rs`) is a *codec dispatcher*, not a wire data carrier. Phase 2 collapses the runtime onto `WireAny*` (the data enum); for Phase 1d we land it as a parallel scaffold so adapters / filters can be written against `&mut WireAnyRequest<'_>` without forcing the runtime migration in the same Phase.

- [ ] **Step 1: Write failing tests**

Create `crates/gw-codec/tests/wire_any.rs`:

```rust
//! WireAny scaffold — verify variants exist and `kind()` matches.

use gw_codec::wire::{WireAnyRequest, WireAnyResponse, WireAnyStreamChunk};
use gw_core::protocol::ProtocolKind;

#[test]
fn request_variant_tag_matches_protocol() {
    let openai_body = br#"{"model":"gpt-4","messages":[]}"#;
    let parsed: gw_codec::openai::wire::OpenAiRequest<'_> =
        serde_json::from_slice(openai_body).expect("parse");
    let wire = WireAnyRequest::OpenAi(parsed);
    assert_eq!(wire.kind(), ProtocolKind::OpenAi);
}

#[test]
fn response_variant_tag_matches_protocol() {
    // Empty enums admit pattern matching even without a constructable value.
    // We assert the variants exist by listing them in a match on an Option.
    fn _typecheck(w: Option<WireAnyResponse<'_>>) -> ProtocolKind {
        match w {
            Some(WireAnyResponse::OpenAi(_)) => ProtocolKind::OpenAi,
            Some(WireAnyResponse::Anthropic(_)) => ProtocolKind::Anthropic,
            Some(WireAnyResponse::Gemini(_)) => ProtocolKind::Gemini,
            None => ProtocolKind::OpenAi,
        }
    }
}

#[test]
fn stream_chunk_variants_exist() {
    fn _typecheck(w: Option<WireAnyStreamChunk<'_>>) -> ProtocolKind {
        match w {
            Some(WireAnyStreamChunk::OpenAi(_)) => ProtocolKind::OpenAi,
            Some(WireAnyStreamChunk::Anthropic(_)) => ProtocolKind::Anthropic,
            Some(WireAnyStreamChunk::Gemini(_)) => ProtocolKind::Gemini,
            None => ProtocolKind::OpenAi,
        }
    }
}
```

- [ ] **Step 2: Run, confirm failure**

```bash
cargo test -p gw-codec --test wire_any
```

Expected: compile error — `gw_codec::wire` doesn't exist yet.

- [ ] **Step 3: Identify the existing wire types**

`grep "^pub struct.*Request" crates/gw-codec/src/openai/wire.rs crates/gw-codec/src/anthropic/wire.rs crates/gw-codec/src/gemini/wire.rs` to confirm the exact struct names. Expected: `OpenAiRequest<'a>`, `AnthropicRequest<'a>`, `GeminiRequest<'a>` for requests; similarly `*Response<'a>` and `*StreamChunk<'a>` (or whatever each module names them). Adjust the variants below if the actual struct names differ.

- [ ] **Step 4: Create `wire/mod.rs`**

```rust
//! `gw-codec::wire` — wire-level data carriers.
//!
//! - [`any`] — `WireAny{Request, Response, StreamChunk}` enums used by
//!   adapter `patch_request` / `patch_response` (A-T0) and by Phase 2's
//!   N² wire→wire matrix.
//!
//! Phase 1d lands these enums as a scaffold. The runtime path still
//! flows through [`crate::AnyCodec`] (a codec dispatcher); Phase 2
//! migrates the proxy hot path onto `WireAny*`.

pub mod any;

pub use any::{WireAnyRequest, WireAnyResponse, WireAnyStreamChunk};
```

- [ ] **Step 5: Create `wire/any.rs`**

```rust
//! WireAny enums — one variant per supported protocol.
//!
//! Adapter `patch_request` / `patch_response` (A-T0) and Phase 2's
//! semantic-trait dispatch (`apply_*_any`) operate on these enums.

use crate::anthropic::wire::{AnthropicRequest, AnthropicResponse, AnthropicStreamChunk};
use crate::gemini::wire::{GeminiRequest, GeminiResponse, GeminiStreamChunk};
use crate::openai::wire::{OpenAiRequest, OpenAiResponse, OpenAiStreamChunk};
use gw_core::protocol::ProtocolKind;

/// Wire-level request, tagged by protocol.
pub enum WireAnyRequest<'a> {
    OpenAi(OpenAiRequest<'a>),
    Anthropic(AnthropicRequest<'a>),
    Gemini(GeminiRequest<'a>),
}

impl WireAnyRequest<'_> {
    pub fn kind(&self) -> ProtocolKind {
        match self {
            Self::OpenAi(_) => ProtocolKind::OpenAi,
            Self::Anthropic(_) => ProtocolKind::Anthropic,
            Self::Gemini(_) => ProtocolKind::Gemini,
        }
    }
}

/// Wire-level response, tagged by protocol.
pub enum WireAnyResponse<'a> {
    OpenAi(OpenAiResponse<'a>),
    Anthropic(AnthropicResponse<'a>),
    Gemini(GeminiResponse<'a>),
}

impl WireAnyResponse<'_> {
    pub fn kind(&self) -> ProtocolKind {
        match self {
            Self::OpenAi(_) => ProtocolKind::OpenAi,
            Self::Anthropic(_) => ProtocolKind::Anthropic,
            Self::Gemini(_) => ProtocolKind::Gemini,
        }
    }
}

/// Wire-level stream chunk, tagged by protocol.
pub enum WireAnyStreamChunk<'a> {
    OpenAi(OpenAiStreamChunk<'a>),
    Anthropic(AnthropicStreamChunk<'a>),
    Gemini(GeminiStreamChunk<'a>),
}

impl WireAnyStreamChunk<'_> {
    pub fn kind(&self) -> ProtocolKind {
        match self {
            Self::OpenAi(_) => ProtocolKind::OpenAi,
            Self::Anthropic(_) => ProtocolKind::Anthropic,
            Self::Gemini(_) => ProtocolKind::Gemini,
        }
    }
}
```

If any of the three modules currently lacks a `pub struct *Response<'a>` or `pub struct *StreamChunk<'a>` type, add the minimum public stub the variant needs — but verify first; the codec crate already has these structs (per `grep "^pub struct" crates/gw-codec/src/*/wire.rs`). If a struct is named differently (e.g., `AnthropicStreamFrame` not `AnthropicStreamChunk`), use the actual name in both the `use` and the variant payload, and document the discrepancy in a one-line comment above the variant. Naming alignment is a Phase 2 cleanup.

- [ ] **Step 6: Declare the module in `gw-codec/src/lib.rs`**

Add `pub mod wire;` to the module list:

```rust
pub mod any;
pub mod anthropic;
pub(crate) mod common;
pub mod gemini;
pub mod openai;
pub mod passthrough;
pub mod trait_def;
pub mod wire;

pub use any::AnyCodec;
pub use trait_def::{Headers, ProtocolCodec};
pub use wire::{WireAnyRequest, WireAnyResponse, WireAnyStreamChunk};
```

- [ ] **Step 7: Run tests + workspace check**

```bash
cargo test -p gw-codec --test wire_any
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add -u
git add crates/gw-codec/src/wire crates/gw-codec/tests/wire_any.rs
git commit -m "$(cat <<'EOF'
feat(gw-codec): scaffold WireAny{Request,Response,StreamChunk} enums

Codec S1 v2 §ADR-S1-v2-6 routes adapter patch_request / patch_response and
Phase 2's apply_*_any dispatchers through WireAny enums (one variant per
protocol). Land the enums + a kind() helper that returns the matching
ProtocolKind.

Runtime path still uses AnyCodec (the codec dispatcher); Phase 2 migrates
the proxy hot path onto WireAny so the wire→wire matrix can dispatch off
these enums without re-tagging.
EOF
)"
```

---

### Task 6: Create the `gw-adapter` crate skeleton

**Files:**
- Create: `crates/gw-adapter/Cargo.toml`
- Create: `crates/gw-adapter/src/lib.rs`
- Modify: `Cargo.toml` (workspace root — add `crates/gw-adapter` to `members`)

- [ ] **Step 1: Add `crates/gw-adapter` to workspace members**

Edit the root `Cargo.toml`. Insert `"crates/gw-adapter",` into the `members = [...]` list — alphabetically that goes right after `"crates/gw-auth",`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/gw-core",
    "crates/gw-codec-macros",
    "crates/gw-codec",
    "crates/gw-store",
    "crates/gw-auth",
    "crates/gw-adapter",
    "crates/gw-routing",
    "crates/gw-ledger",
    "crates/gw-health",
    "crates/gw-proxy",
    "crates/gw-gateway",
]
```

- [ ] **Step 2: Create `crates/gw-adapter/Cargo.toml`**

```toml
[package]
name = "gw-adapter"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
gw-core = { path = "../gw-core" }
gw-codec = { path = "../gw-codec" }
gw-health = { path = "../gw-health" }
async-trait.workspace = true
thiserror.workspace = true
secrecy.workspace = true
dashmap.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt", "test-util"] }
```

If `async-trait` is not yet a workspace dep (it likely is — `grep '"0.1' Cargo.toml` to confirm), add it to the root `[workspace.dependencies]` block. The existing root `Cargo.toml` shows `async-trait = "0.1"`, so it should already be there.

- [ ] **Step 3: Create `crates/gw-adapter/src/lib.rs`**

```rust
//! `gw-adapter` — vendor-specific upstream rewrite hooks.
//!
//! Per integration spec §3 C-1 this is a **business-layer** crate. It
//! provides the `UpstreamAdapter` v2 trait (4 hooks + `transport_profile`),
//! an `AdapterCtx` runtime-agnostic carrier, a `NoopAdapter` default that
//! preserves the legacy `x-api-key` injection behavior, an `AdapterRegistry`
//! for vendor lookup, and an `UpstreamErrorAnalyzer` trait skeleton (full
//! K-B vendor analyzers ship in Phase 3).
//!
//! Spec: `docs/superpowers/specs/2026-05-19-adapter-a-t0-trait-v2.md`.

pub mod analyzer;
pub mod ctx;
pub mod error;
pub mod noop;
pub mod registry;
pub mod trait_def;

pub use analyzer::{DefaultErrorAnalyzer, UpstreamErrorAnalyzer};
pub use ctx::{AdapterCtx, HeadersAccessor, HeadersMut, UrlAccessor, UrlMut};
pub use error::AdapterError;
pub use noop::NoopAdapter;
pub use registry::AdapterRegistry;
pub use trait_def::{ChromeVersion, TransportProfile, UpstreamAdapter};
```

The five submodules are added in Tasks 7-9; this lib.rs forward-declares them so each task can land independently. Create empty stubs now so the crate compiles:

```bash
for f in analyzer ctx error noop registry trait_def; do
    touch crates/gw-adapter/src/$f.rs
done
```

Then fill `trait_def.rs` with a placeholder so the `pub use` lines resolve:

```rust
// crates/gw-adapter/src/trait_def.rs — replaced in Task 7
pub struct ChromeVersion;
pub struct TransportProfile;
pub trait UpstreamAdapter {}
```

Repeat for the other five files with the minimum stubs the `pub use` in `lib.rs` requires. Each Task 7/8/9 then replaces its file with the real content.

(Alternative: collapse Tasks 6-9 into the same commit if you'd rather not ship stub modules. Plan keeps them separate so each is small + reviewable.)

- [ ] **Step 4: Verify `cargo check`**

```bash
cargo check --workspace
```

Expected: green. New crate compiles to nothing useful; nothing depends on it.

- [ ] **Step 5: Verify layering invariant**

```bash
bash scripts/check-invariants.sh
```

Expected: `[crate_layering]` still passes. `gw-adapter` is business-layer, depends on supporting (`gw-core`, `gw-codec`) + business (`gw-health`) — that's allowed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/gw-adapter
git commit -m "$(cat <<'EOF'
feat(gw-adapter): new business-layer crate skeleton

Per integration spec §3 C-1, gw-adapter is the second business-layer crate
(gw-health was first, landed in Phase 1c). Adds the workspace member +
empty module skeleton for trait_def / ctx / error / noop / analyzer /
registry. Each module is fleshed out in Tasks 7-9.

Adapter v2 design: docs/superpowers/specs/2026-05-19-adapter-a-t0-trait-v2.md.
EOF
)"
```

---

### Task 7: `UpstreamAdapter` trait + `AdapterError` + `TransportProfile`

**Files:**
- Modify: `crates/gw-adapter/src/trait_def.rs`
- Modify: `crates/gw-adapter/src/error.rs`
- Test: inline `#[cfg(test)]` in `trait_def.rs`

- [ ] **Step 1: Write the trait + supporting types**

Replace `crates/gw-adapter/src/trait_def.rs`:

```rust
//! `UpstreamAdapter` v2 — 4 hooks + `transport_profile` declaration.
//!
//! Spec: A-T0 §ADR-AT0-1. Each vendor adapter (claude_code / gemini_cli /
//! vertex_ai / antigravity / codex / kiro) implements this trait; the proxy
//! and the task runtime call into it via `AdapterRegistry::for_vendor`.

use crate::ctx::AdapterCtx;
use crate::error::AdapterError;
use async_trait::async_trait;
use gw_codec::wire::{WireAnyRequest, WireAnyResponse};

#[async_trait]
pub trait UpstreamAdapter: Send + Sync + 'static {
    /// Adapter identifier (e.g. `"claude_code"`, `"vertex_ai"`).
    fn name(&self) -> &'static str;

    /// Rewrite auth headers (OAuth bearer, SA JWT, x-api-key, etc.).
    /// Async because OAuth refresh may be needed (K-C).
    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError>;

    /// Rewrite URL / host / TLS profile. Synchronous — availability probes
    /// are cached.
    fn rewrite_transport(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError>;

    /// Modify the outgoing request wire via WireAny semantic dispatch.
    /// Default: no-op.
    fn patch_request(
        &self,
        _ctx: &AdapterCtx<'_>,
        _wire: &mut WireAnyRequest<'_>,
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Modify the incoming response wire. Default: no-op.
    fn patch_response(
        &self,
        _ctx: &AdapterCtx<'_>,
        _wire: &mut WireAnyResponse<'_>,
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Browser-fingerprint declaration (A12). `None` = standard reqwest /
    /// pingora; `Some(profile)` = the http-client selector picks an
    /// impersonate-capable backend. Full backend selection lands in a
    /// future spec; in v1 a non-`None` value warns and falls back to
    /// `Standard`.
    fn transport_profile(&self) -> Option<TransportProfile> {
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportProfile {
    Standard,
    ChromeImpersonate { version: ChromeVersion },
    FirefoxImpersonate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromeVersion {
    V110,
    V116,
    V119,
    V120,
    V123,
    V124,
    V131,
}
```

- [ ] **Step 2: Write the error enum**

Replace `crates/gw-adapter/src/error.rs`:

```rust
//! `AdapterError` — vendor-adapter failure modes.
//!
//! Each variant maps onto a `gw-core::error::codes::GW_*` code at the
//! call site (gw-proxy / gw-task-runtime translate). Adapter code itself
//! stays runtime-agnostic.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("oauth: {0}")]
    Oauth(String),

    #[error("transport rewrite failed: {0}")]
    Transport(String),

    #[error("request patch failed: {0}")]
    PatchRequest(String),

    #[error("response patch failed: {0}")]
    PatchResponse(String),

    #[error("vendor configuration missing: {0}")]
    MissingConfig(&'static str),

    /// Upstream signaled the key is permanently invalid (e.g. revoked
    /// OAuth token discovered during refresh). The caller is responsible
    /// for issuing the K-A cooldown — adapter code stays side-effect-free
    /// (per A-T0 §future-q-7).
    #[error("upstream signaled key invalid")]
    KeyInvalid,
}
```

The full `OauthError` type lives in `gw-health` per K-C and isn't required for Phase 1d; we use `String` as the inner payload so the variant compiles today.

- [ ] **Step 3: Add a smoke test for the trait surface**

Append to `crates/gw-adapter/src/trait_def.rs`:

```rust
#[cfg(test)]
mod trait_surface_tests {
    use super::*;

    struct DummyAdapter;

    #[async_trait]
    impl UpstreamAdapter for DummyAdapter {
        fn name(&self) -> &'static str { "dummy" }
        async fn rewrite_auth(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> { Ok(()) }
        fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> { Ok(()) }
    }

    #[test]
    fn dummy_compiles_and_defaults_patch_hooks_to_noop() {
        let d = DummyAdapter;
        assert_eq!(d.name(), "dummy");
        assert!(d.transport_profile().is_none());
    }
}
```

(This test cannot exercise the async hooks because constructing `AdapterCtx` is the next task. The trait's surface is exercised by the type-check + the `transport_profile` default.)

- [ ] **Step 4: Run tests**

```bash
cargo test -p gw-adapter
```

Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-adapter/src/trait_def.rs crates/gw-adapter/src/error.rs
git commit -m "$(cat <<'EOF'
feat(gw-adapter): UpstreamAdapter trait + AdapterError + TransportProfile

Per A-T0 §ADR-AT0-1, UpstreamAdapter has 4 hooks:
- rewrite_auth (async; OAuth refresh path)
- rewrite_transport (sync; URL / TLS profile)
- patch_request (default no-op; mutates WireAnyRequest)
- patch_response (default no-op; mutates WireAnyResponse)
plus a transport_profile() declaration for A12 (browser fingerprint).

AdapterError carries 6 variants mapping onto gw-core::error::codes GW_*
codes at the call site. The OauthError inner type lands in Phase 3 with
K-C OauthCache; today we use String to keep the crate decoupled.
EOF
)"
```

---

### Task 8: `AdapterCtx` + `HeadersMut` / `UrlMut` + accessor traits

**Files:**
- Modify: `crates/gw-adapter/src/ctx.rs`
- Test: `crates/gw-adapter/tests/headers_url_roundtrip.rs` (create)

- [ ] **Step 1: Write the ctx module**

Replace `crates/gw-adapter/src/ctx.rs`:

```rust
//! `AdapterCtx` — the runtime-agnostic context handed to every adapter hook.
//!
//! Per A-T0 §ADR-AT0-2 the adapter sees only `dyn HeadersAccessor` and
//! `dyn UrlAccessor` views of the outbound request. The proxy / task
//! runtime implements these traits over their native types
//! (pingora::http::RequestHeader, reqwest::RequestBuilder, …) at the
//! call site.

use gw_core::entity::VendorKey;
use gw_core::ids::{KeyId, UserId, VendorId};
use gw_core::ids::ModelName;
use gw_core::protocol::{ApiFormat, ProtocolKind};
use gw_health::store::KeyHealthStore;
use std::sync::Arc;

pub struct AdapterCtx<'a> {
    pub vendor_id: VendorId,
    pub vendor_key_id: KeyId,
    pub vendor_key: Arc<VendorKey>,

    pub client_protocol: ProtocolKind,
    pub upstream_protocol: ProtocolKind,
    pub model: ModelName,

    pub user_id: UserId,
    pub api_format: ApiFormat,

    /// K-A handle — adapter reads health snapshots (e.g. circuit state)
    /// but never writes back (only K-B vendor analyzers and the logging
    /// filter do).
    pub health_store: &'a Arc<KeyHealthStore>,

    pub headers_mut: HeadersMut<'a>,
    pub url_mut: UrlMut<'a>,

    pub trace_id: &'a str,
    pub request_id: &'a str,
}

pub struct HeadersMut<'a> {
    pub inner: &'a mut dyn HeadersAccessor,
}

pub trait HeadersAccessor: Send {
    fn get(&self, name: &str) -> Option<&str>;
    fn set(&mut self, name: &str, value: &str);
    fn remove(&mut self, name: &str);
}

impl HeadersMut<'_> {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.inner.get(name)
    }
    pub fn set(&mut self, name: &str, value: &str) {
        self.inner.set(name, value);
    }
    pub fn remove(&mut self, name: &str) {
        self.inner.remove(name);
    }
}

pub struct UrlMut<'a> {
    pub inner: &'a mut dyn UrlAccessor,
}

pub trait UrlAccessor: Send {
    fn scheme(&self) -> &str;
    fn host(&self) -> &str;
    fn path(&self) -> &str;
    fn set_scheme(&mut self, scheme: &str);
    fn set_host(&mut self, host: &str);
    fn set_path(&mut self, path: &str);
}

impl UrlMut<'_> {
    pub fn scheme(&self) -> &str { self.inner.scheme() }
    pub fn host(&self)   -> &str { self.inner.host() }
    pub fn path(&self)   -> &str { self.inner.path() }
    pub fn set_scheme(&mut self, s: &str) { self.inner.set_scheme(s); }
    pub fn set_host(&mut self,   h: &str) { self.inner.set_host(h); }
    pub fn set_path(&mut self,   p: &str) { self.inner.set_path(p); }
}
```

Verify the imports compile — `gw_core::ids::UserId`, `ModelName`, and `gw_core::protocol::ApiFormat` should all exist (ApiFormat was added in Phase 1c). If `UserId` doesn't exist as a `pub` newtype, check `crates/gw-core/src/ids.rs` and pick the existing user-identifier newtype name; substitute accordingly. **Do not introduce a new id type here** — that's a `gw-core` decision.

The `oauth_cache` field from the spec is deferred: the `OauthCache` type lives in `gw-health` per K-C and lands in Phase 3. Phase 1d's `AdapterCtx` does not include it; Phase 3 adds the field when `OauthCache` materialises. Document this in a one-line comment above the struct:

```rust
/// Phase 1d carries health_store only; oauth_cache (K-C) is added in Phase 3.
pub struct AdapterCtx<'a> { ... }
```

- [ ] **Step 2: Write a round-trip test for the trait objects**

Create `crates/gw-adapter/tests/headers_url_roundtrip.rs`:

```rust
//! Verify HeadersMut / UrlMut behave as thin pass-throughs over their
//! accessor traits. This exercises the dyn-dispatch path the proxy and
//! task runtime use.

use gw_adapter::{HeadersAccessor, HeadersMut, UrlAccessor, UrlMut};
use std::collections::HashMap;

struct InMemoryHeaders(HashMap<String, String>);

impl HeadersAccessor for InMemoryHeaders {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(|s| s.as_str())
    }
    fn set(&mut self, name: &str, value: &str) {
        self.0.insert(name.to_owned(), value.to_owned());
    }
    fn remove(&mut self, name: &str) {
        self.0.remove(name);
    }
}

struct InMemoryUrl {
    scheme: String,
    host: String,
    path: String,
}

impl UrlAccessor for InMemoryUrl {
    fn scheme(&self) -> &str { &self.scheme }
    fn host(&self)   -> &str { &self.host }
    fn path(&self)   -> &str { &self.path }
    fn set_scheme(&mut self, s: &str) { self.scheme = s.to_owned(); }
    fn set_host(&mut self,   h: &str) { self.host = h.to_owned(); }
    fn set_path(&mut self,   p: &str) { self.path = p.to_owned(); }
}

#[test]
fn headers_set_get_remove_roundtrip() {
    let mut backing = InMemoryHeaders(HashMap::new());
    let mut h = HeadersMut { inner: &mut backing };
    h.set("x-api-key", "secret");
    assert_eq!(h.get("x-api-key"), Some("secret"));
    h.remove("x-api-key");
    assert_eq!(h.get("x-api-key"), None);
}

#[test]
fn url_mutation_visible_through_view() {
    let mut backing = InMemoryUrl {
        scheme: "http".into(),
        host: "example.com".into(),
        path: "/v1/chat".into(),
    };
    let mut u = UrlMut { inner: &mut backing };
    u.set_scheme("https");
    u.set_host("api.openai.com");
    u.set_path("/v1/responses");
    assert_eq!(u.scheme(), "https");
    assert_eq!(u.host(), "api.openai.com");
    assert_eq!(u.path(), "/v1/responses");
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p gw-adapter
```

Expected: 2 new tests pass.

- [ ] **Step 4: Commit**

```bash
git add -u
git add crates/gw-adapter/tests/headers_url_roundtrip.rs
git commit -m "$(cat <<'EOF'
feat(gw-adapter): AdapterCtx + HeadersMut/UrlMut trait objects

Per A-T0 §ADR-AT0-2, adapters see the outbound request as
dyn HeadersAccessor + dyn UrlAccessor — the proxy adapts pingora's
RequestHeader; the task runtime will adapt reqwest in Phase 6. AdapterCtx
carries vendor / key / user ids, the client/upstream protocols, the model,
api_format (Bm5), trace / request ids, and a &Arc<KeyHealthStore> for K-A
reads.

OauthCache is deferred to Phase 3 (lands with K-C); add a comment marker
so Phase 3 knows where to insert the field.
EOF
)"
```

---

### Task 9: `NoopAdapter` + `AdapterRegistry` + analyzer trait scaffold

**Files:**
- Modify: `crates/gw-adapter/src/noop.rs`
- Modify: `crates/gw-adapter/src/analyzer.rs`
- Modify: `crates/gw-adapter/src/registry.rs`
- Test: `crates/gw-adapter/tests/noop_behavior.rs` (create)
- Test: `crates/gw-adapter/tests/registry_lookup.rs` (create)

- [ ] **Step 1: Implement `NoopAdapter`**

Replace `crates/gw-adapter/src/noop.rs`:

```rust
//! `NoopAdapter` — the default `UpstreamAdapter` impl.
//!
//! Equivalent to the legacy hard-coded x-api-key injection currently in
//! `gw-proxy::filters::upstream`. Phase 3 wires the registry through the
//! proxy + deletes the hard-coded path.

use crate::ctx::AdapterCtx;
use crate::error::AdapterError;
use crate::trait_def::UpstreamAdapter;
use async_trait::async_trait;
use secrecy::ExposeSecret;

pub struct NoopAdapter;

#[async_trait]
impl UpstreamAdapter for NoopAdapter {
    fn name(&self) -> &'static str { "noop" }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let key = ctx.vendor_key.api_key.expose_secret().to_owned();
        ctx.headers_mut.remove("authorization");
        ctx.headers_mut.set("x-api-key", &key);
        Ok(())
    }

    fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        Ok(())
    }
}
```

- [ ] **Step 2: Implement the analyzer scaffold**

Replace `crates/gw-adapter/src/analyzer.rs`:

```rust
//! `UpstreamErrorAnalyzer` — vendor-aware classification of upstream
//! responses into `ErrorOutcome` (full K-B impl in Phase 3).
//!
//! Phase 1d lands the trait surface + a `DefaultErrorAnalyzer` that
//! returns "uncategorized" for everything. Phase 3 fills in the vendor
//! analyzers (claude_code 401, gemini_cli quota header, codex retry-after,
//! …) and the rate-limit info extraction.

/// Outcome of analyzing an upstream HTTP response.
///
/// Phase 1d places this in `gw-adapter` so the trait can compile; Phase 3
/// K-B may move it to `gw-health` if it grows fields shared with the
/// health store. Either crate is allowed per integration spec §3 C-7.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ErrorOutcome {
    /// `None` = uncategorized. Phase 3 introduces the full enum.
    pub kind: Option<ErrorKind>,
    /// Retry-After hint from the upstream (seconds).
    pub retry_after_secs: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Authentication,
    RateLimit,
    Quota,
    UpstreamServer,
    Transient,
    Unknown,
}

pub trait UpstreamErrorAnalyzer: Send + Sync + 'static {
    /// Analyze an upstream response; return what the proxy / runtime
    /// should do next.
    fn analyze(
        &self,
        status: u16,
        body_head: &[u8],
        headers: &dyn HeaderView,
    ) -> ErrorOutcome;
}

/// Read-only header view passed to analyzers. Decoupled from the
/// adapter-side `HeadersAccessor` because analysis runs on responses;
/// the proxy / runtime supplies its own adapter at the call site.
pub trait HeaderView {
    fn get(&self, name: &str) -> Option<&str>;
}

pub struct DefaultErrorAnalyzer;

impl UpstreamErrorAnalyzer for DefaultErrorAnalyzer {
    fn analyze(
        &self,
        status: u16,
        _body_head: &[u8],
        headers: &dyn HeaderView,
    ) -> ErrorOutcome {
        // Phase 1d skeleton: only the universally-safe Retry-After header
        // is parsed. Vendor-specific extraction (gemini quota headers,
        // codex headers, etc.) lands in Phase 3.
        let retry_after_secs = headers
            .get("retry-after")
            .and_then(|s| s.parse::<u32>().ok());
        let kind = match status {
            401 | 403 => Some(ErrorKind::Authentication),
            429       => Some(ErrorKind::RateLimit),
            500..=599 => Some(ErrorKind::UpstreamServer),
            _         => None,
        };
        ErrorOutcome { kind, retry_after_secs }
    }
}
```

- [ ] **Step 3: Implement the registry**

Replace `crates/gw-adapter/src/registry.rs`:

```rust
//! `AdapterRegistry` — vendor-id → adapter + error-analyzer lookup.
//!
//! Built at gateway / worker bootstrap; one shared instance per process.

use crate::analyzer::{DefaultErrorAnalyzer, UpstreamErrorAnalyzer};
use crate::noop::NoopAdapter;
use crate::trait_def::UpstreamAdapter;
use dashmap::DashMap;
use gw_core::ids::VendorId;
use std::sync::Arc;

pub struct AdapterRegistry {
    by_vendor: DashMap<VendorId, Arc<dyn UpstreamAdapter>>,
    default_adapter: Arc<NoopAdapter>,
    error_analyzers: DashMap<VendorId, Arc<dyn UpstreamErrorAnalyzer>>,
    default_error_analyzer: Arc<DefaultErrorAnalyzer>,
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self {
            by_vendor: DashMap::new(),
            default_adapter: Arc::new(NoopAdapter),
            error_analyzers: DashMap::new(),
            default_error_analyzer: Arc::new(DefaultErrorAnalyzer),
        }
    }

    pub fn register_adapter(&self, vendor: VendorId, adapter: Arc<dyn UpstreamAdapter>) {
        self.by_vendor.insert(vendor, adapter);
    }

    pub fn register_error_analyzer(
        &self,
        vendor: VendorId,
        analyzer: Arc<dyn UpstreamErrorAnalyzer>,
    ) {
        self.error_analyzers.insert(vendor, analyzer);
    }

    pub fn for_vendor(&self, v: &VendorId) -> Arc<dyn UpstreamAdapter> {
        self.by_vendor
            .get(v)
            .map(|e| e.value().clone())
            .unwrap_or_else(|| (self.default_adapter.clone()) as Arc<dyn UpstreamAdapter>)
    }

    pub fn error_analyzer_for(&self, v: &VendorId) -> Arc<dyn UpstreamErrorAnalyzer> {
        self.error_analyzers
            .get(v)
            .map(|e| e.value().clone())
            .unwrap_or_else(|| (self.default_error_analyzer.clone()) as Arc<dyn UpstreamErrorAnalyzer>)
    }
}
```

- [ ] **Step 4: Write Noop behavior test**

Create `crates/gw-adapter/tests/noop_behavior.rs`:

```rust
//! NoopAdapter must perform exactly the legacy x-api-key injection:
//! drop any existing `authorization`, set `x-api-key` to the vendor key's
//! secret material. Nothing else.

use gw_adapter::{AdapterCtx, HeadersAccessor, HeadersMut, NoopAdapter, UpstreamAdapter, UrlMut};
use gw_adapter::ctx::{UrlAccessor};
use gw_core::entity::{KeyStatus, VendorKey};
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::protocol::ProtocolKind;
use gw_health::store::KeyHealthStore;
use secrecy::SecretString;
use std::collections::HashMap;
use std::sync::Arc;

struct InMemoryHeaders(HashMap<String, String>);

impl HeadersAccessor for InMemoryHeaders {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(|s| s.as_str())
    }
    fn set(&mut self, name: &str, value: &str) {
        self.0.insert(name.to_owned(), value.to_owned());
    }
    fn remove(&mut self, name: &str) {
        self.0.remove(name);
    }
}

struct InMemoryUrl;

impl UrlAccessor for InMemoryUrl {
    fn scheme(&self) -> &str { "https" }
    fn host(&self)   -> &str { "example.com" }
    fn path(&self)   -> &str { "/" }
    fn set_scheme(&mut self, _: &str) {}
    fn set_host(&mut self,   _: &str) {}
    fn set_path(&mut self,   _: &str) {}
}

fn dummy_vendor_key() -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from("test-key".to_string()),
        vendor_id: VendorId::from("test-vendor".to_string()),
        api_key: SecretString::from("sk-test-123".to_string()),
        accessible_models: vec![],
        model_mapping: Default::default(),
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority: 0,
        weight: 0,
        rpm_limit: None,
        tpm_limit: None,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: vec![],
        expires_at: None,
        status: KeyStatus::Active,
        tags: vec![],
    })
}

#[tokio::test]
async fn noop_injects_x_api_key_and_removes_authorization() {
    let health = Arc::new(KeyHealthStore::default());
    let mut hdrs = InMemoryHeaders({
        let mut m = HashMap::new();
        m.insert("authorization".into(), "Bearer stale".into());
        m
    });
    let mut url = InMemoryUrl;

    let mut ctx = AdapterCtx {
        vendor_id: VendorId::from("test-vendor".to_string()),
        vendor_key_id: KeyId::from("test-key".to_string()),
        vendor_key: dummy_vendor_key(),
        client_protocol: ProtocolKind::OpenAi,
        upstream_protocol: ProtocolKind::OpenAi,
        model: ModelName::from("gpt-4".to_string()),
        user_id: UserId::from("u-1".to_string()),
        api_format: ProtocolKind::OpenAi,
        health_store: &health,
        headers_mut: HeadersMut { inner: &mut hdrs },
        url_mut: UrlMut { inner: &mut url },
        trace_id: "trace-1",
        request_id: "req-1",
    };

    let n = NoopAdapter;
    n.rewrite_auth(&mut ctx).await.expect("rewrite_auth");

    assert_eq!(hdrs.0.get("x-api-key").map(|s| s.as_str()), Some("sk-test-123"));
    assert!(!hdrs.0.contains_key("authorization"));
}
```

If any `VendorKey` field name in `gw-core::entity::VendorKey` differs from what is listed above, mirror the actual struct shape — *do not* invent fields. Use `cat crates/gw-core/src/entity.rs | grep -A 30 'pub struct VendorKey'` to verify.

- [ ] **Step 5: Write registry lookup test**

Create `crates/gw-adapter/tests/registry_lookup.rs`:

```rust
//! AdapterRegistry returns Noop for unregistered vendors and returns the
//! exact registered adapter when present.

use gw_adapter::{AdapterCtx, AdapterError, AdapterRegistry, UpstreamAdapter, UpstreamErrorAnalyzer};
use gw_adapter::analyzer::{ErrorOutcome, HeaderView};
use async_trait::async_trait;
use gw_core::ids::VendorId;
use std::sync::Arc;

struct MarkerAdapter;

#[async_trait]
impl UpstreamAdapter for MarkerAdapter {
    fn name(&self) -> &'static str { "marker" }
    async fn rewrite_auth(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> { Ok(()) }
    fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> { Ok(()) }
}

struct MarkerAnalyzer;
impl UpstreamErrorAnalyzer for MarkerAnalyzer {
    fn analyze(&self, _status: u16, _body: &[u8], _h: &dyn HeaderView) -> ErrorOutcome {
        ErrorOutcome::default()
    }
}

#[test]
fn lookup_returns_noop_when_unregistered() {
    let r = AdapterRegistry::default();
    let v = VendorId::from("unknown".to_string());
    assert_eq!(r.for_vendor(&v).name(), "noop");
}

#[test]
fn lookup_returns_registered_adapter() {
    let r = AdapterRegistry::default();
    let v = VendorId::from("marker-vendor".to_string());
    r.register_adapter(v.clone(), Arc::new(MarkerAdapter));
    assert_eq!(r.for_vendor(&v).name(), "marker");
}

#[test]
fn analyzer_lookup_falls_back_to_default() {
    let r = AdapterRegistry::default();
    let v = VendorId::from("unknown".to_string());
    let analyzer = r.error_analyzer_for(&v);
    struct Empty;
    impl HeaderView for Empty {
        fn get(&self, _: &str) -> Option<&str> { None }
    }
    let outcome = analyzer.analyze(503, &[], &Empty);
    assert!(outcome.kind.is_some());
}

#[test]
fn registered_analyzer_overrides_default() {
    let r = AdapterRegistry::default();
    let v = VendorId::from("marker-vendor".to_string());
    r.register_error_analyzer(v.clone(), Arc::new(MarkerAnalyzer));
    let analyzer = r.error_analyzer_for(&v);
    struct Empty;
    impl HeaderView for Empty {
        fn get(&self, _: &str) -> Option<&str> { None }
    }
    let outcome = analyzer.analyze(503, &[], &Empty);
    assert_eq!(outcome.kind, None);
}
```

- [ ] **Step 6: Run tests**

```bash
cargo test -p gw-adapter
```

Expected: all 6 tests pass (2 from Task 8 + 1 from Task 7 + 1 noop + 4 registry — actually 4 here, total 7; counts may vary if you split tests).

- [ ] **Step 7: Workspace check**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green. `gw-adapter` is a self-contained business-layer crate; nothing depends on it yet.

- [ ] **Step 8: Commit**

```bash
git add -u
git add crates/gw-adapter/tests/noop_behavior.rs crates/gw-adapter/tests/registry_lookup.rs
git commit -m "$(cat <<'EOF'
feat(gw-adapter): NoopAdapter + AdapterRegistry + UpstreamErrorAnalyzer scaffold

NoopAdapter performs the legacy x-api-key injection (removes any
authorization header, sets x-api-key from the vendor key secret) — bit-for-bit
equivalent to the hard-coded path in gw-proxy::filters::upstream that Phase 3
deletes when wiring the registry through.

AdapterRegistry holds two DashMaps (vendor → adapter, vendor → analyzer) +
default fallbacks. for_vendor / error_analyzer_for never return None;
unregistered vendors get the defaults.

UpstreamErrorAnalyzer trait + DefaultErrorAnalyzer (Phase 1d skeleton:
parses Retry-After + maps HTTP status to ErrorKind). Vendor analyzers
(claude_code 401, gemini_cli quota header, codex retry-after) land in
Phase 3 K-B.
EOF
)"
```

---

### Task 10: Activate lints + CLAUDE.md updates + Living amendment

**Files:**
- Modify: `scripts/check-invariants.sh`
- Modify: `CLAUDE.md`
- Modify: `docs/superpowers/specs/2026-05-20-v2-integration-design.md` §6

- [ ] **Step 1: Activate `no_legacy_retry` and `no_patch_body` lints**

In `scripts/check-invariants.sh`, find the `PENDING lints` section near the bottom. **Move** these two blocks out of the comment section into the live region (above the `# PENDING lints` banner), removing the leading `#`:

```bash
# ---------------------------------------------------------------------------
# no_legacy_retry (§5.2) — retry.rs deleted; failover lives in Phase 4 R-E.
# ---------------------------------------------------------------------------
echo "[no_legacy_retry] gw-routing/src/retry.rs must not exist"
if [ -f crates/gw-routing/src/retry.rs ]; then
  bad "crates/gw-routing/src/retry.rs still present"
else
  ok "no retry.rs"
fi

# ---------------------------------------------------------------------------
# no_patch_body (§5.2) — adapter trait v2 has no patch_body / BodyPatch.
# ---------------------------------------------------------------------------
echo "[no_patch_body] patch_body / BodyPatch must not appear"
if grep -rnE '\bfn patch_body\b|\bBodyPatch::' crates/ 2>/dev/null; then
  bad "patch_body / BodyPatch remains"
else
  ok "no patch_body / BodyPatch references"
fi
```

Leave `no_ir_runtime_struct` and `no_gateway_addr_config` and `deploy_e2e_matrix` in the PENDING block — those activate in Phase 2 / Phase 6 respectively.

- [ ] **Step 2: Run the invariant script**

```bash
bash scripts/check-invariants.sh
```

Expected: `all active invariants hold`. The count goes from 9 active lints to 11.

- [ ] **Step 3: Update CLAUDE.md — workspace dep diagram**

Find the `## Workspace dependency direction` section. Add a `gw-adapter ◄── gw-proxy (binary)` row above the `gw-proxy` row, and adjust the `gw-health` row to also be referenced by `gw-adapter`:

```
gw-core ◄── everything (IR, IDs, error codes)
gw-codec-macros ◄── gw-codec
gw-codec        ◄── gw-proxy, gw-adapter
gw-store        ◄── gw-auth, gw-routing, gw-ledger, gw-proxy, gw-gateway
gw-health       ◄── gw-routing, gw-proxy, gw-gateway, gw-adapter
gw-adapter      ◄── (Phase 3 wires it through gw-proxy)
gw-auth         ◄── gw-proxy
gw-routing      ◄── gw-proxy
gw-ledger       ◄── gw-proxy
gw-proxy        ◄── gw-gateway (binary)
```

(Adjust to match the file's exact ASCII art conventions.)

- [ ] **Step 4: Update CLAUDE.md — Crate layering rules (v2 integration)**

In the "Business layer" bullet, add `gw-adapter` if not already mentioned:

```
- **Business layer** (`gw-adapter`, `gw-health`, `gw-routing`, `gw-task`):
  one crate per business concern. ...
```

(`gw-adapter` is already in the textual list per the existing CLAUDE.md; verify and adjust only if missing.)

- [ ] **Step 5: Add a note about A10 deferral**

In the `## Things that are intentional, not bugs` section, append a new bullet:

```
- **`gw-core::ir` not renamed to `gw-core::schema` yet.** Integration spec §2.1 A10 prescribes the rename + `#[non_exhaustive]` strip. Phase 1d landed the *scaffolding* parts of S1 v2 (WireAny enums, wire_convert_matrix! proc-macro skeleton, Mutations upgrade) without breaking the runtime IR path. The strict A10 form — module rename, removal of public constructors / Default impl / Serialize-Deserialize — lands atomically in Phase 2 alongside the wire→wire matrix migration. The `no_ir_runtime_struct` lint stays PENDING in `scripts/check-invariants.sh` until then.
```

- [ ] **Step 6: Append Living amendment to integration spec §6**

Edit `docs/superpowers/specs/2026-05-20-v2-integration-design.md`. Find the `## §6 表 E — Living amendments` section. Append after the existing two entries:

```
- 2026-05-XX  A10 strict-form re-timing: schema rename + non_exhaustive strip moves to Phase 2
  Why: A10 prescribes `gw-core::ir → gw-core::schema` + `#[non_exhaustive]` on every IR struct, which forbids the public Default/constructor calls that the runtime via.rs handwritten From/TryFrom impls currently rely on. Phase 1d cannot strip those without simultaneously migrating the runtime onto wire→wire (codec S1 v2 N² conversions), which is Phase 2's scope. The Phase 1d-shipped pieces of S1 v2 (Mutations upgrade, WireAny enum, wire_convert_matrix! proc-macro scaffold) are framework-only and do not touch the runtime IR path. Phase 2 lands the rename + strip atomically with the wire→wire migration; the `no_ir_runtime_struct` lint activates at Phase 2 exit.
  How to apply: Phase 2 plan must include (1) the module rename, (2) the constructor strip, (3) activation of `no_ir_runtime_struct` in scripts/check-invariants.sh. Phase 1 exit gate text in §4.2 is read as "scaffolding complete + A1/A8/A9/A11 done; A10 strict form re-timed to Phase 2 exit per this amendment".
```

(Replace `XX` with the actual date when the commit lands.)

- [ ] **Step 7: Run the full gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
bash scripts/check-invariants.sh
```

Expected: all green. Invariant report shows `all active invariants hold` with the two new lints in the output.

- [ ] **Step 8: Commit**

```bash
git add scripts/check-invariants.sh CLAUDE.md docs/superpowers/specs/2026-05-20-v2-integration-design.md
git commit -m "$(cat <<'EOF'
ci+docs: activate no_legacy_retry + no_patch_body; A10 re-timing amendment

scripts/check-invariants.sh: promote no_legacy_retry and no_patch_body from
PENDING to active (9 → 11 active lints).

CLAUDE.md: workspace dep diagram gains gw-adapter; "Things intentional"
gains an A10 deferral note (schema rename lands in Phase 2 with the
wire→wire migration, not Phase 1d).

Integration spec §6: append Living amendment formalizing the A10 strict-form
re-timing from Phase 1d to Phase 2 exit gate. Same pattern as Bm2's
re-timing in Phase 1c.
EOF
)"
```

---

## Self-Review

**Spec coverage check** — every Phase 1 deliverable in integration spec §4.1 either lands here or is registered as deferred:

| Deliverable | Status |
|---|---|
| gw-adapter trait v2 + Noop + Registry | Tasks 6-9 |
| gw-health crate (KeyHealthStore + SlidingWindow) | Phase 1c (done) |
| gw-ledger FreezeGuard trait + RequestFreeze + TaskFreeze placeholder | Phase 1b (done) |
| codec S1 v2 — Mutations upgrade | Task 3 |
| codec S1 v2 — wire_convert_matrix! 宏框架 | Task 4 |
| codec S1 v2 — WireAny enum | Task 5 |
| codec S1 v2 — gw-core::schema rename + non_exhaustive | re-timed to Phase 2 (Living amendment, Task 10) |
| gw-core::usage::UsageSnapshot | Phase 1b (done) |
| Delete gw-routing::retry.rs (A1) | Task 1 |
| Delete gw-routing::breaker.rs (A2) | Phase 1c (done) |
| Delete gw-billing residue (A7) | Phase 1a (done — verified no Cargo.toml ref) |
| Delete BillingRecord.cost_micro (A4) | Phase 1b (done) |
| Delete FreezeGuard::settle(actual) (A3) | Phase 1b (done) |
| Delete IrConvert derives (A9) | Task 2 |
| Delete RetryConfig::mode (A11) | Task 1 |
| Delete patch_body / BodyPatch (A8) | trivially satisfied + lint (Tasks 6-10) |
| CLAUDE.md three layering rules logged | Phase 1a (done) + amendment Task 10 |
| business-split-refactor.md plan v1 residue cleaned (Bm6) | Phase 1a (done) |

**Placeholder scan** — searched for "TBD", "TODO" without context, "implement later". Found one TODO in Task 4 step 5 explicitly marked "phase-2" — that's the trybuild upgrade note. Kept as it documents the deferred work.

**Type consistency check:**
- `Mutations::set_*` bit positions are sequential 0..=8.
- `WireAnyRequest / WireAnyResponse / WireAnyStreamChunk` follow the same OpenAi/Anthropic/Gemini variant order.
- `AdapterCtx` field names match the spec (A-T0 §ADR-AT0-2) where they exist in Phase 1d; the deferred `oauth_cache` field is documented inline.
- `UpstreamAdapter` method signatures match A-T0 §ADR-AT0-1 exactly.
- `ErrorOutcome` and `ErrorKind` are owned by `gw-adapter` in Phase 1d; the Task 9 comment leaves the Phase 3 ownership question open (could move to `gw-health` if K-B needs shared state).
- `KeyHealthStore` is referenced via `gw-health::store::KeyHealthStore` in `AdapterCtx::health_store` — verify this matches the actual Phase 1c export.

**Cross-task dependency check:**
- Task 4 requires Task 2's placeholder file. Task 2 explicitly creates the placeholder.
- Task 5 requires the existing wire structs (`OpenAiRequest`, etc.). Already present; Task 2 only removes the inert derives.
- Tasks 7-9 require Task 6's empty module stubs. Task 6 creates them explicitly.
- Task 8's test references `gw_adapter::ctx::UrlAccessor` — confirmed exported via `lib.rs` in Task 6.
- Task 10 activates lints that match the changes from Tasks 1 + 6-9.

**Build-green discipline check** — Tasks 1, 2, 3, 4, 5, 7, 8, 9 each leave the workspace green at their commit boundary. Task 6 ships stub modules that compile to nothing (`pub struct ChromeVersion;` etc.) — those stubs are replaced in Tasks 7-9 before the trait surface is used externally, so no caller sees the stubs.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-20-phase-1d-codec-and-adapter-scaffold.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, two-stage review (spec compliance → code quality) between tasks, fast iteration. Same pattern that landed Phase 1b and Phase 1c.

**2. Inline Execution** — Execute tasks in this session using `superpowers:executing-plans`, batch execution with checkpoints for your review.

Which approach?
