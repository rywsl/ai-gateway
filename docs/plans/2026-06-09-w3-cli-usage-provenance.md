# W3 CLI-Extracted Usage Provenance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a first-class `cli_extracted` per-dimension usage provenance tag so CLI-parsed usage is distinguishable from upstream final, upstream partial, estimates, crash fallback, and unknown usage.

**Architecture:** Keep the retired `UsageSource` deleted and extend the current `ProvenanceTag` / `UsageProvenance` model. CLI body and stream extraction should stamp dimensions with `ProvenanceTag::CliExtracted`, and the shared rank order must be reflected in Rust merge logic, Go consumer contract docs, and the Python soak dedupe oracle.

**Tech Stack:** Rust 1.88 workspace, serde `snake_case` enums, Pingora response filter, `gw-core`, `gw-ledger`, `gw-proxy`, `gw-gateway` metrics catalog, Redis `usage:records` contract, Python soak tests.

---

## User Decision Needed

W0 recommends this final priority order:

```text
upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown
```

Recommended decision: approve the W0 ordering above.

Reason: CLI usage is parsed from upstream CLI wire response bodies/events, so it is more authoritative than partial IR accumulation and tokenizer estimates. It must still lose to a terminal usage payload from a standard upstream codec.

If the user rejects this priority, stop before implementation and update the rank tables in this plan before touching code.

## File Responsibilities

- Modify: `crates/gw-core/src/usage.rs`
  - Add `ProvenanceTag::CliExtracted`, serialized as `"cli_extracted"`.
  - Add a focused serde test for the new wire value.
- Modify: `crates/gw-ledger/tests/usage_schema.rs`
  - Lock `cli_extracted` serialization in the ledger schema tests.
  - Ensure a `UsageRecord` can carry `cli_extracted` in `usage_provenance`.
- Modify: `crates/gw-proxy/src/filters/response.rs`
  - Update `copy_provenance` ranking to `UpstreamFinal=5`, `CliExtracted=4`, `UpstreamPartial=3`, `Estimated=2`, `CrashFallback=1`, `Unknown=0`.
  - Stamp CLI full-response and stream-delta extraction into `ctx.usage_provenance`.
  - Add unit tests for rank promotion and CLI stamping.
- Read only: `crates/gw-proxy/src/ctx.rs`
  - Use the existing `ReqCtx::default()` test constructor; do not modify this file for W3.
- Read only: `crates/gw-gateway/src/metrics_catalog.rs`
  - Verify `gw_cli_usage_extract_total` is still registered once; do not add another metric or change the catalog for W3.
- Modify: `tests/e2e/client/soak/dedupe.py`
  - Update docstring and `_PROVENANCE_RANK` to match the approved priority.
- Modify: `tests/e2e/client/soak/test_kill_loop.py`
  - Add a rank-order unit test for `cli_extracted`.
- Modify: `docs/spec/01-protocol-codec.md`
  - Replace the "Not built" note for `UsageSource::CliExtracted` with the as-built `cli_extracted` provenance behavior.
- Modify: `docs/spec/06-billing-ledger.md`
  - Document the expanded `ProvenanceTag` set and dedupe rank order.
- Modify: `docs/go-contracts/parts/02-go-as-consumer.md`
  - Update `usage_provenance` schema example and Go settler dedupe priority.
- Modify: `docs/ROADMAP.md`
  - Move or remove the W3 CLI provenance open item according to the repo's existing ROADMAP closure style.

## Task 1: Lock The Wire Enum Contract

**Files:**
- Modify: `crates/gw-core/src/usage.rs`
- Modify: `crates/gw-ledger/tests/usage_schema.rs`

- [ ] **Step 1: Add failing `gw-core` serde test**

Add this test inside `#[cfg(test)] mod tests` in `crates/gw-core/src/usage.rs`:

```rust
#[test]
fn provenance_tag_cli_extracted_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&ProvenanceTag::CliExtracted).unwrap(),
        "\"cli_extracted\""
    );
}
```

- [ ] **Step 2: Run the focused failing core test**

Run:

```bash
cargo test -p gw-core provenance_tag_cli_extracted_serializes_snake_case --locked
```

Expected before implementation:

```text
error[E0599]: no variant or associated item named `CliExtracted` found for enum `ProvenanceTag`
```

- [ ] **Step 3: Add the new enum variant**

Modify `crates/gw-core/src/usage.rs`:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceTag {
    /// Vendor sent terminal usage event/payload.
    UpstreamFinal,
    /// Parsed from an upstream CLI response body or stream event.
    CliExtracted,
    /// Parsed mid-stream; never saw final.
    UpstreamPartial,
    /// Fell back to tokenizer/estimate.
    Estimated,
    /// InflightLedger reconciler-emitted.
    CrashFallback,
    /// Upstream produced nothing or context is half-residual.
    #[default]
    Unknown,
}
```

- [ ] **Step 4: Verify the core enum test passes**

Run:

```bash
cargo test -p gw-core provenance_tag_cli_extracted_serializes_snake_case --locked
```

Expected after implementation:

```text
test usage::tests::provenance_tag_cli_extracted_serializes_snake_case ... ok
```

- [ ] **Step 5: Add failing ledger schema assertions**

In `crates/gw-ledger/tests/usage_schema.rs`, extend `provenance_tag_serializes_snake_case`:

```rust
assert_eq!(
    serde_json::to_string(&ProvenanceTag::CliExtracted).unwrap(),
    "\"cli_extracted\""
);
```

In `test_record()`, change one dimension to prove `UsageRecord` carries the new value:

```rust
usage_provenance: UsageProvenance {
    input: ProvenanceTag::UpstreamFinal,
    output: ProvenanceTag::CliExtracted,
    cache_read: ProvenanceTag::UpstreamFinal,
    cache_creation: ProvenanceTag::Unknown,
    thinking: ProvenanceTag::Unknown,
    multimodal_input: ProvenanceTag::Unknown,
},
```

In `schema_serializes_new_fields`, add:

```rust
assert!(
    json.contains("\"output\":\"cli_extracted\""),
    "output provenance label"
);
```

- [ ] **Step 6: Run the focused ledger schema test**

Run:

```bash
cargo test -p gw-ledger --test usage_schema --locked
```

Expected after Task 1 implementation:

```text
test provenance_tag_serializes_snake_case ... ok
test schema_serializes_new_fields ... ok
```

## Task 2: Stamp CLI Extraction Provenance In The Proxy

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs`

- [ ] **Step 1: Add a failing rank-promotion unit test**

Add this test to the existing `#[cfg(test)]` module in `crates/gw-proxy/src/filters/response.rs` or create one at the bottom of the file if no response-filter test module exists:

```rust
#[test]
fn cli_extracted_ranks_between_final_and_partial() {
    use gw_core::usage::ProvenanceTag;

    assert!(provenance_rank(ProvenanceTag::UpstreamFinal) > provenance_rank(ProvenanceTag::CliExtracted));
    assert!(provenance_rank(ProvenanceTag::CliExtracted) > provenance_rank(ProvenanceTag::UpstreamPartial));
    assert!(provenance_rank(ProvenanceTag::UpstreamPartial) > provenance_rank(ProvenanceTag::Estimated));
    assert!(provenance_rank(ProvenanceTag::Estimated) > provenance_rank(ProvenanceTag::CrashFallback));
    assert!(provenance_rank(ProvenanceTag::CrashFallback) > provenance_rank(ProvenanceTag::Unknown));
}
```

This test intentionally requires the implementation to expose a private helper:

```rust
fn provenance_rank(t: gw_core::usage::ProvenanceTag) -> u8
```

- [ ] **Step 2: Run the failing rank test**

Run:

```bash
cargo test -p gw-proxy cli_extracted_ranks_between_final_and_partial --locked
```

Expected before implementation:

```text
error[E0425]: cannot find function `provenance_rank` in this scope
```

- [ ] **Step 3: Implement the shared rank helper**

Replace the nested `rank` helper inside `copy_provenance` with this file-level helper:

```rust
fn provenance_rank(t: gw_core::usage::ProvenanceTag) -> u8 {
    use gw_core::usage::ProvenanceTag;
    match t {
        ProvenanceTag::UpstreamFinal => 5,
        ProvenanceTag::CliExtracted => 4,
        ProvenanceTag::UpstreamPartial => 3,
        ProvenanceTag::Estimated => 2,
        ProvenanceTag::CrashFallback => 1,
        ProvenanceTag::Unknown => 0,
    }
}
```

Update `copy_provenance` so `promote` uses `provenance_rank`:

```rust
fn copy_provenance(e: &gw_codec::usage_extractor::ExtractedUsage, ctx: &mut ReqCtx) {
    use gw_core::usage::ProvenanceTag;

    fn promote(dst: &mut ProvenanceTag, src: ProvenanceTag) {
        if provenance_rank(src) > provenance_rank(*dst) {
            *dst = src;
        }
    }

    promote(&mut ctx.usage_provenance.input, e.provenance.input);
    promote(&mut ctx.usage_provenance.output, e.provenance.output);
    promote(
        &mut ctx.usage_provenance.cache_read,
        e.provenance.cache_read,
    );
    promote(
        &mut ctx.usage_provenance.cache_creation,
        e.provenance.cache_creation,
    );
    promote(&mut ctx.usage_provenance.thinking, e.provenance.thinking);
    promote(
        &mut ctx.usage_provenance.multimodal_input,
        e.provenance.multimodal_input,
    );
}
```

Update the comment above `copy_provenance`:

```rust
/// Promote per-dimension provenance: only advance if the incoming tag has
/// higher confidence. Order: UpstreamFinal > CliExtracted > UpstreamPartial >
/// Estimated > CrashFallback > Unknown.
```

- [ ] **Step 4: Add a helper to stamp CLI-provided dimensions**

Add this helper near `apply_cli_full_usage` / `apply_cli_delta`:

```rust
fn mark_cli_extracted_usage(
    ctx: &mut ReqCtx,
    input: bool,
    output: bool,
    cache_read: bool,
    cache_creation: bool,
) {
    use gw_core::usage::ProvenanceTag;

    fn promote(dst: &mut ProvenanceTag) {
        if provenance_rank(ProvenanceTag::CliExtracted) > provenance_rank(*dst) {
            *dst = ProvenanceTag::CliExtracted;
        }
    }

    if input {
        promote(&mut ctx.usage_provenance.input);
    }
    if output {
        promote(&mut ctx.usage_provenance.output);
    }
    if cache_read {
        promote(&mut ctx.usage_provenance.cache_read);
    }
    if cache_creation {
        promote(&mut ctx.usage_provenance.cache_creation);
    }
}
```

- [ ] **Step 5: Stamp non-stream CLI full usage**

Update `apply_cli_full_usage` so it stamps the four dimensions extracted from `CliUsageFull`:

```rust
fn apply_cli_full_usage(shape: ApiShape, body: &[u8], ctx: &mut ReqCtx) {
    if let Some(full) = cli_usage_extractor(shape).extract_response_usage(body) {
        record_cli_usage_extract(shape, "response_body");
        ctx.usage.input_tokens = full.input_tokens as u32;
        ctx.usage.output_tokens = full.output_tokens as u32;
        ctx.usage.cached_tokens = Some(full.cache_read_input_tokens as u32);
        ctx.usage.cache_creation_tokens = Some(full.cache_creation_input_tokens as u32);
        mark_cli_extracted_usage(ctx, true, true, true, true);
    }
}
```

- [ ] **Step 6: Stamp stream CLI deltas only for dimensions present in the delta**

Update `apply_cli_delta`:

```rust
fn apply_cli_delta(delta: gw_codec::cli::CliUsageDelta, ctx: &mut ReqCtx) {
    let mut has_input = false;
    let mut has_output = false;
    let mut has_cache_read = false;
    let mut has_cache_creation = false;

    if let Some(v) = delta.input_tokens {
        ctx.usage.input_tokens = v as u32;
        has_input = true;
    }
    if let Some(v) = delta.output_tokens {
        ctx.usage.output_tokens = v as u32;
        has_output = true;
    }
    if let Some(v) = delta.cache_read_input_tokens {
        ctx.usage.cached_tokens = Some(v as u32);
        has_cache_read = true;
    }
    if let Some(v) = delta.cache_creation_input_tokens {
        ctx.usage.cache_creation_tokens = Some(v as u32);
        has_cache_creation = true;
    }

    mark_cli_extracted_usage(
        ctx,
        has_input,
        has_output,
        has_cache_read,
        has_cache_creation,
    );
}
```

- [ ] **Step 7: Add response-filter tests for stamping**

Add tests that construct a context with `let mut ctx = ReqCtx::default();`. The assertions must be exactly:

```rust
assert_eq!(ctx.usage_provenance.input, ProvenanceTag::CliExtracted);
assert_eq!(ctx.usage_provenance.output, ProvenanceTag::CliExtracted);
assert_eq!(ctx.usage_provenance.cache_read, ProvenanceTag::CliExtracted);
assert_eq!(ctx.usage_provenance.cache_creation, ProvenanceTag::CliExtracted);
assert_eq!(ctx.usage_provenance.thinking, ProvenanceTag::Unknown);
assert_eq!(ctx.usage_provenance.multimodal_input, ProvenanceTag::Unknown);
```

Use two test names:

```rust
#[test]
fn cli_full_usage_marks_extracted_provenance()

#[test]
fn cli_delta_marks_only_present_dimensions()
```

For the delta test, use a delta with only `output_tokens: Some(9)` and all other dimensions `None`; expected output provenance is `CliExtracted`, expected input/cache dimensions remain `Unknown`.

- [ ] **Step 8: Run proxy-focused tests**

Run:

```bash
cargo test -p gw-proxy cli_extracted_ranks_between_final_and_partial --locked
cargo test -p gw-proxy cli_full_usage_marks_extracted_provenance --locked
cargo test -p gw-proxy cli_delta_marks_only_present_dimensions --locked
```

Expected after implementation:

```text
test ... cli_extracted_ranks_between_final_and_partial ... ok
test ... cli_full_usage_marks_extracted_provenance ... ok
test ... cli_delta_marks_only_present_dimensions ... ok
```

## Task 3: Update Soak Dedupe Oracle

**Files:**
- Modify: `tests/e2e/client/soak/dedupe.py`
- Modify: `tests/e2e/client/soak/test_kill_loop.py`

- [ ] **Step 1: Add a failing Python assertion for the new priority**

Add this test to `tests/e2e/client/soak/test_kill_loop.py`:

```python
def test_dedupe_rank_orders_cli_extracted_between_final_and_partial():
    from soak.dedupe import _PROVENANCE_RANK

    assert _PROVENANCE_RANK["upstream_final"] == 5
    assert _PROVENANCE_RANK["cli_extracted"] == 4
    assert _PROVENANCE_RANK["upstream_partial"] == 3
    assert _PROVENANCE_RANK["estimated"] == 2
    assert _PROVENANCE_RANK["crash_fallback"] == 1
    assert _PROVENANCE_RANK["unknown"] == 0
```

- [ ] **Step 2: Run the failing Python test**

Run:

```bash
PYTHONPATH=tests/e2e/client pytest tests/e2e/client/soak/test_kill_loop.py::test_dedupe_rank_orders_cli_extracted_between_final_and_partial -q
```

Expected before implementation:

```text
KeyError: 'cli_extracted'
```

- [ ] **Step 3: Update the soak rank table and docstring**

In `tests/e2e/client/soak/dedupe.py`, update the docstring priority list:

```text
upstream_final > cli_extracted > upstream_partial > estimated >
crash_fallback > unknown
```

Update `_PROVENANCE_RANK`:

```python
_PROVENANCE_RANK: dict[str, int] = {
    "upstream_final": 5,
    "cli_extracted": 4,
    "upstream_partial": 3,
    "estimated": 2,
    "crash_fallback": 1,
    "unknown": 0,
}
```

- [ ] **Step 4: Run soak unit tests**

Run:

```bash
PYTHONPATH=tests/e2e/client pytest tests/e2e/client/soak/test_kill_loop.py -q
```

Expected after implementation:

```text
7 passed
```

If the file already contains more than seven tests when this plan is executed, expected result is all tests in `test_kill_loop.py` pass.

## Task 4: Update Specs And Go Consumer Contract

**Files:**
- Modify: `docs/spec/01-protocol-codec.md`
- Modify: `docs/spec/06-billing-ledger.md`
- Modify: `docs/go-contracts/parts/02-go-as-consumer.md`
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: Update protocol codec spec**

In `docs/spec/01-protocol-codec.md`, replace the ROADMAP note:

```markdown
- **`UsageSource::CliExtracted`** — no `UsageSource` enum exists in `gw-core`; there is no typed marker distinguishing CLI-extracted usage from IR-accumulated usage.
```

with:

```markdown
- **CLI usage provenance** — `UsageSource` remains deleted. CLI response-body and stream-event extractors stamp extracted dimensions with `usage_provenance.<dim> = "cli_extracted"` while continuing to increment `gw_cli_usage_extract_total{shape,source}`. The precedence order is `upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown`.
```

- [ ] **Step 2: Update billing ledger spec**

In `docs/spec/06-billing-ledger.md`, update the `UsageRecord schema` bullet for `usage_provenance` to:

```markdown
- `usage_provenance: UsageProvenance` — per-dimension `ProvenanceTag` (`input`/`output`/`cache_read`/`cache_creation`/`thinking`/`multimodal_input`) frozen at emission. Tags serialize as `upstream_final`, `cli_extracted`, `upstream_partial`, `estimated`, `crash_fallback`, and `unknown`.
```

Add this deliberate behavior after the per-attempt records bullet:

```markdown
- **CLI-extracted usage outranks partial and estimated usage.** CLI body/stream extractors parse usage emitted by upstream CLI wire traffic, so their dimensions are tagged `cli_extracted`. Merge and Go dedupe priority is `upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown`.
```

- [ ] **Step 3: Update Go usage payload example**

In `docs/go-contracts/parts/02-go-as-consumer.md` §2.1.2, update the inline comment next to `usage_provenance.input`:

```json
"input": "upstream_final",      // upstream_final | cli_extracted | upstream_partial | estimated | crash_fallback | unknown
```

- [ ] **Step 4: Update Go dedupe priority section**

In `docs/go-contracts/parts/02-go-as-consumer.md` §2.1.8, replace the priority block with:

```text
优先级（从高到低）：
  upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown
```

Add the numeric table immediately below it:

```markdown
Go settler rank:

| provenance | rank |
|---|---:|
| `upstream_final` | 5 |
| `cli_extracted` | 4 |
| `upstream_partial` | 3 |
| `estimated` | 2 |
| `crash_fallback` | 1 |
| `unknown` | 0 |
```

- [ ] **Step 5: Update ROADMAP closure**

Find the W3 / CLI provenance open item in `docs/ROADMAP.md`:

```bash
rg -n "UsageSource::CliExtracted|CliExtracted|cli_extracted|usage_provenance" docs/ROADMAP.md
```

Apply the repo's existing closure pattern. The resulting ROADMAP must no longer say the CLI-extracted provenance marker is missing. It should point to `docs/spec/01-protocol-codec.md`, `docs/spec/06-billing-ledger.md`, and `docs/go-contracts/parts/02-go-as-consumer.md` as the as-built contract.

- [ ] **Step 6: Run spec checks**

Run:

```bash
bash scripts/check-spec.sh docs/spec/01-protocol-codec.md
bash scripts/check-spec.sh docs/spec/06-billing-ledger.md
```

Expected:

```text
OK docs/spec/01-protocol-codec.md
OK docs/spec/06-billing-ledger.md
```

If `check-spec.sh` prints a different success format in the current checkout, expected result is exit code 0 for both commands.

## Task 5: Integration Verification

**Files:**
- Read: all modified files from Tasks 1-4

- [ ] **Step 1: Run focused Rust tests**

Run:

```bash
cargo test -p gw-core usage --locked
cargo test -p gw-ledger --test usage_schema --locked
cargo test -p gw-proxy cli --locked
cargo test -p gw-gateway metrics_catalog --locked
```

Expected:

```text
cargo test -p gw-core usage --locked
# all usage tests pass

cargo test -p gw-ledger --test usage_schema --locked
# all usage_schema tests pass

cargo test -p gw-proxy cli --locked
# all CLI response-filter tests pass

cargo test -p gw-gateway metrics_catalog --locked
# catalog tests pass, with no duplicate gw_cli_usage_extract_total registration
```

- [ ] **Step 2: Run soak dedupe tests**

Run:

```bash
PYTHONPATH=tests/e2e/client pytest tests/e2e/client/soak/test_kill_loop.py -q
```

Expected:

```text
all tests in test_kill_loop.py pass
```

- [ ] **Step 3: Run contract/spec checks**

Run:

```bash
bash scripts/check-spec.sh docs/spec/01-protocol-codec.md
bash scripts/check-spec.sh docs/spec/06-billing-ledger.md
```

Expected:

```text
both commands exit 0
```

- [ ] **Step 4: Run formatting and invariant gates**

Run:

```bash
cargo fmt --all -- --check
bash scripts/check-invariants.sh
git diff --check
```

Expected:

```text
cargo fmt --all -- --check
# exit 0

bash scripts/check-invariants.sh
# exit 0

git diff --check
# no whitespace errors
```

## Task 6: Commit

**Files:**
- Commit all files changed by implementation tasks.

- [ ] **Step 1: Review final diff**

Run:

```bash
git diff -- crates/gw-core/src/usage.rs \
  crates/gw-ledger/tests/usage_schema.rs \
  crates/gw-proxy/src/filters/response.rs \
  crates/gw-gateway/src/metrics_catalog.rs \
  tests/e2e/client/soak/dedupe.py \
  tests/e2e/client/soak/test_kill_loop.py \
  docs/spec/01-protocol-codec.md \
  docs/spec/06-billing-ledger.md \
  docs/go-contracts/parts/02-go-as-consumer.md \
  docs/ROADMAP.md
```

Expected:

```text
Only CLI usage provenance code, tests, docs, and soak oracle changes appear.
No `UsageSource` enum or `usage_source` JSON field is reintroduced.
No duplicate `gw_cli_usage_extract_total` metric is added.
```

- [ ] **Step 2: Commit implementation**

Run:

```bash
git add crates/gw-core/src/usage.rs \
  crates/gw-ledger/tests/usage_schema.rs \
  crates/gw-proxy/src/filters/response.rs \
  crates/gw-gateway/src/metrics_catalog.rs \
  tests/e2e/client/soak/dedupe.py \
  tests/e2e/client/soak/test_kill_loop.py \
  docs/spec/01-protocol-codec.md \
  docs/spec/06-billing-ledger.md \
  docs/go-contracts/parts/02-go-as-consumer.md \
  docs/ROADMAP.md
git commit -m "feat(ledger): record cli-extracted usage provenance"
```

Expected:

```text
[branch <sha>] feat(ledger): record cli-extracted usage provenance
```

## Final Verification Matrix

Run these before handing off implementation:

```bash
cargo test -p gw-core usage --locked
cargo test -p gw-ledger --test usage_schema --locked
cargo test -p gw-proxy cli --locked
cargo test -p gw-gateway metrics_catalog --locked
PYTHONPATH=tests/e2e/client pytest tests/e2e/client/soak/test_kill_loop.py -q
bash scripts/check-spec.sh docs/spec/01-protocol-codec.md
bash scripts/check-spec.sh docs/spec/06-billing-ledger.md
cargo fmt --all -- --check
bash scripts/check-invariants.sh
git diff --check
```

Expected final state:

- `ProvenanceTag::CliExtracted` serializes as `"cli_extracted"`.
- CLI response-body extraction stamps extracted dimensions as `cli_extracted`.
- CLI stream-delta extraction stamps only dimensions present in the delta as `cli_extracted`.
- `copy_provenance` keeps `upstream_final` over `cli_extracted` and promotes `cli_extracted` over `upstream_partial`.
- Go contract and soak dedupe rank order match W0 exactly: `5,4,3,2,1,0`.
- `gw_cli_usage_extract_total{shape,source}` remains the single CLI extraction metric.
- No `UsageSource` type or `usage_source` field is reintroduced.
