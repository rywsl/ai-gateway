# W9 Kiro CLI Sample Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unblock Kiro CLI implementation by collecting a real traffic sample bundle, then implement exact CLI detection, passthrough semantics, and usage extraction without claiming Kiro CLI is already implemented.

**Architecture:** Kiro CLI remains a dedicated CLI shape under `ApiShape::KiroCli`; it is not Kiro EventStream and runtime behavior must not be inferred from `ProtocolKind::Kiro`. The first lane captures and reviews real Kiro CLI traffic, including usage-bearing responses and errors; only after that evidence exists may the implementation lane add the codec module, detector markers, proxy usage extractor, fixtures, tests, and roadmap closure.

**Tech Stack:** Rust workspace crates `gw-core`, `gw-codec`, `gw-proxy`; existing `CliCodec` / `CliUsageExtractor` contracts; `rg`, focused `cargo test` commands only after samples are available.

---

## Current Blocker

Kiro CLI is currently blocked because the repository has no real Kiro CLI traffic sample. Existing docs and code register `ApiShape::KiroCli` as a shape boundary, while marker detection, usage extraction, and passthrough behavior remain unimplemented. This plan is the implementation path after the sample blocker is removed; it must not be used to close Kiro CLI roadmap entries before sample review and usage settlement are complete.

## Decisions And Stop Conditions

- Kiro CLI is not Kiro EventStream. Keep `ApiShape::KiroCli` independent from `ApiShape::KiroEventStream`; never infer CLI request or response behavior from `ProtocolKind::Kiro` alone.
- `ProtocolKind::Kiro` may remain the shared coarse protocol string, but CLI runtime dispatch must choose by `ApiShape::KiroCli`, detector evidence, and CLI codec behavior.
- If the Kiro CLI response usage field is absent, always zero, hidden in terminal metadata, or otherwise not extractable from captured traffic, do not close the roadmap item.
- If usage has no real extractable fields, obtain a non-zero fallback or settlement decision before roadmap closure. Acceptable outcomes are a documented estimate source, a billing fallback contract, or a product decision that explicitly settles non-zero billing for Kiro CLI.
- If only non-stream traffic is captured, stop before implementing stream usage extraction.
- If only stream traffic is captured, stop before implementing non-stream full-response usage extraction.
- If only successful responses are captured, stop before documenting error passthrough behavior as complete.
- If sample markers overlap an existing CLI detector in `crates/gw-codec/src/cli/mod.rs`, stop and define deterministic detection precedence before implementation.

## Minimum Sample Bundle

Capture each item below from real Kiro CLI traffic. Store raw artifacts outside the repository until secrets are redacted; commit only redacted fixtures and a short evidence note.

- Request method, path, and query string for stream and non-stream calls.
- Request headers with redaction: replace bearer tokens, API keys, session ids, AWS credentials, cookies, authorization signatures, machine ids, account ids, and user identifiers with stable placeholders such as `<redacted-token-1>`.
- Request body head sufficient for early detection, plus full request body when it contains CLI-only fields, model fields, stream flags, request ids, or background/task flags.
- Stream framing details: raw chunk boundaries, SSE or non-SSE prefixes, event names, event type fields, terminal chunks, keepalive frames, and any final metadata frame.
- Terminal usage data for stream responses, including exact field names, nesting, units, and whether usage arrives in a final event, trailer-like chunk, or separate message.
- Non-stream usage data, including exact JSON path, token field names, unit semantics, and whether missing fields occur on valid success responses.
- Error response sample, including status code, headers, body shape, CLI-specific error fields, and whether error bodies are stream-framed.
- Comparison sample for OpenAI `/v1/responses` with `background:true`, including method/path/query, redacted headers, body head, full body, response status, and usage-bearing response shape.
- Comparison sample for OpenAI CLI traffic, including marker headers/body fields and usage-bearing terminal response, so Kiro CLI markers are not guessed from OpenAI CLI behavior.

## Files After Samples Are Available

- Create: `crates/gw-codec/src/cli/kiro_cli.rs` for Kiro CLI shape identity, hint extraction, passthrough defaults, TTFT event types, and usage extractor binding.
- Modify: `crates/gw-codec/src/cli/mod.rs` to export the Kiro CLI module and add exact `detect_cli` marker checks using only sample-proven headers/path/body-head inputs.
- Modify: `crates/gw-proxy/src/filters/response.rs` to replace the Kiro CLI noop usage extractor path with the codec-owned extractor.
- Create or modify: focused fixture files under the existing fixture/test convention after inspecting current test layout.
- Modify: `docs/spec/01-protocol-codec.md`, `docs/ROADMAP.md`, and relevant go-contract docs only after detector, usage extraction, passthrough behavior, and fallback/settlement decisions are implemented and verified.

## Task 1: Sample Intake And Redaction

**Files:**
- Create: `docs/superpowers/evidence/2026-06-10-w9-kiro-cli-samples.md`
- Create: redacted fixture files under the repository's existing fixture location selected after `rg --files | rg 'fixture|sample|golden'`

Operator helper: `scripts/capture_kiro_cli_samples/README.md` documents offline mitmproxy sample capture and `python3 scripts/capture_kiro_cli_samples/extract.py --input <raw-flow-outside-repo> --output-dir <redacted-output>`.

- [ ] **Step 1: Inspect existing fixture layout**

Run:

```bash
rg --files | rg 'fixture|fixtures|sample|samples|golden'
```

Expected: Output identifies the convention to reuse for redacted request/response fixtures, or no output confirms a new focused fixture directory is needed.

- [ ] **Step 2: Capture and redact the sample bundle**

Create the evidence note with this structure and fill every field from captured traffic:

```markdown
# W9 Kiro CLI Sample Evidence

## Redaction Rules

- Authorization and bearer tokens are replaced with `<redacted-token-N>`.
- API keys, session ids, cookies, signatures, machine ids, account ids, and user identifiers are replaced with stable `<redacted-...-N>` placeholders.
- Header names, JSON field names, event names, status codes, paths, query keys, and token-count numbers are preserved.

## Kiro CLI Stream Request

- Method:
- Path:
- Query:
- Headers:
- Body head:
- Full body fixture:

## Kiro CLI Stream Response

- Status:
- Headers:
- Framing:
- Event types:
- Terminal usage:
- Fixture:

## Kiro CLI Non-Stream Request

- Method:
- Path:
- Query:
- Headers:
- Body head:
- Full body fixture:

## Kiro CLI Non-Stream Response

- Status:
- Headers:
- Usage fields:
- Fixture:

## Kiro CLI Error Response

- Status:
- Headers:
- Body shape:
- Fixture:

## OpenAI Responses Background Comparison

- Method:
- Path:
- Query:
- Headers:
- Body head:
- Full body fixture:
- Response usage shape:

## OpenAI CLI Comparison

- Marker headers:
- Marker body fields:
- Terminal usage:
- Fixture:
```

Expected: Evidence records concrete captured values and fixture paths. No section contains placeholder language or guesses.

- [ ] **Step 3: Verify usage decision readiness**

Run:

```bash
rg -n "Terminal usage:|Usage fields:|fallback|settlement|non-zero" docs/superpowers/evidence/2026-06-10-w9-kiro-cli-samples.md
```

Expected: Output shows real terminal and non-stream usage evidence, or a documented non-zero fallback/settlement decision. If neither exists, stop before implementation.

## Task 2: Kiro CLI Codec Module

**Files:**
- Create: `crates/gw-codec/src/cli/kiro_cli.rs`
- Modify: `crates/gw-codec/src/cli/mod.rs`
- Test: existing or new `crates/gw-codec/tests/*` file selected from current CLI test layout

- [ ] **Step 1: Write codec identity and hints tests**

Add tests that assert `KiroCliCodec.shape() == ApiShape::KiroCli`, `ApiShape::KiroCli.is_cli()`, and `ApiShape::KiroCli.format_id() == "kiro:cli"`. Add hint tests using exact sample body/path/header fields from Task 1, including model and stream flags only if they exist in the real sample.

Expected: Tests fail because `KiroCliCodec` is not implemented or not exported.

- [ ] **Step 2: Implement `kiro_cli` codec module**

Create `crates/gw-codec/src/cli/kiro_cli.rs` with a focused `KiroCliCodec` that implements the existing `CliCodec` trait. Keep request and response passthrough zero-copy unless the sample proves a required transform. Bind a Kiro CLI usage extractor only after Task 4 defines sample-proven usage paths.

Expected: Codec identity and hints tests pass without changing Kiro EventStream codec behavior.

- [ ] **Step 3: Export the module without detector inference**

Update `crates/gw-codec/src/cli/mod.rs` to export `kiro_cli`. Do not add a detector branch until Task 3 exact markers are written from sample evidence.

Expected: `rg -n "pub mod kiro_cli|KiroCliCodec" crates/gw-codec/src/cli crates/gw-codec/tests` shows module export and focused tests only.

## Task 3: Exact CLI Detection Markers

**Files:**
- Modify: `crates/gw-codec/src/cli/mod.rs`
- Test: existing or new CLI detector tests under `crates/gw-codec/tests/` or module tests in `crates/gw-codec/src/cli/mod.rs`

- [ ] **Step 1: Add sample-proven detector tests**

Write detector tests using the exact Kiro CLI method/path/header/body-head-visible markers from Task 1. Include negative tests for Kiro EventStream headers such as `content-type: application/vnd.amazon.eventstream`, and for OpenAI CLI comparison markers.

Expected: Positive Kiro CLI detector test fails, while negative tests document the required boundary.

- [ ] **Step 2: Implement detector branch**

Add a `detect_cli` branch that returns `Some(ApiShape::KiroCli)` only for exact sample-proven markers. Keep Kiro EventStream out of `detect_cli`; EventStream remains owned by `AnyCodec::Kiro` and `ApiShape::KiroEventStream`.

Expected: Detector tests pass, and no code path treats all `ProtocolKind::Kiro` traffic as CLI traffic.

- [ ] **Step 3: Verify no protocol-level inference**

Run:

```bash
rg -n "ProtocolKind::Kiro.*KiroCli|KiroCli.*ProtocolKind::Kiro|ApiShape::KiroCli" crates/gw-codec crates/gw-proxy crates/gw-core
```

Expected: Matches show shape registration and explicit CLI handling only; no detector or usage code infers CLI behavior solely from `ProtocolKind::Kiro`.

## Task 4: Proxy Usage Extractor Replacement

**Files:**
- Modify: `crates/gw-codec/src/cli/kiro_cli.rs`
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Test: focused proxy or codec usage tests selected from existing CLI usage test layout

- [ ] **Step 1: Write stream usage extractor tests**

Using Task 1 stream fixtures, assert the extractor emits a non-zero `CliUsageDelta` only when the sample terminal usage event arrives. Assert intermediate content chunks do not fabricate usage.

Expected: Test fails while the Kiro CLI extractor is absent or noop.

- [ ] **Step 2: Write non-stream usage extractor tests**

Using Task 1 non-stream fixtures, assert the extractor emits a non-zero `CliUsageFull` from the exact JSON path captured in the sample. Include a valid no-usage fixture only if the fallback/settlement decision in Task 1 permits non-zero settlement outside the response body.

Expected: Test fails while the Kiro CLI extractor is absent or noop.

- [ ] **Step 3: Implement sample-proven extractor**

Implement the Kiro CLI extractor with structured parsing of the exact sample fields. Preserve unknown fields by ignoring them. Do not reuse OpenAI or EventStream usage extraction unless the sample proves identical field names, units, and framing.

Expected: Codec usage tests pass and produce non-zero usage for the captured stream and non-stream success fixtures.

- [ ] **Step 4: Replace proxy noop usage path**

Replace the `KiroCliNoopExtractor` path in `crates/gw-proxy/src/filters/response.rs` with the codec-owned Kiro CLI extractor. Keep Kiro EventStream usage behavior unchanged.

Expected: `rg -n "KiroCliNoopExtractor|KiroNoopUsageExtractor|ApiShape::KiroCli" crates/gw-proxy/src/filters/response.rs` shows no Kiro CLI noop path remains; any Kiro EventStream noop or standard codec path is still explicit.

## Task 5: Fixtures, Regression Tests, And Docs Closure

**Files:**
- Modify: Kiro CLI fixture files created in Task 1
- Modify: `docs/spec/01-protocol-codec.md`
- Modify: `docs/ROADMAP.md`
- Modify: relevant `docs/go-contracts/*` files if shape, usage, or billing contract text changes

- [ ] **Step 1: Add fixture regression coverage**

Ensure each redacted Kiro CLI fixture is consumed by at least one test: stream request detector, stream terminal usage, non-stream full usage, error passthrough, and negative EventStream boundary.

Expected: `rg -n "kiro.*fixture|KiroCli|kiro:cli" crates/gw-codec crates/gw-proxy` shows every fixture path referenced by tests.

- [ ] **Step 2: Run focused tests**

Run only focused checks:

```bash
cargo test -p gw-codec kiro_cli
cargo test -p gw-proxy kiro_cli
```

Expected: Both commands pass. Do not run long full-workspace cargo builds as part of this plan unless the main agent requests them separately.

- [ ] **Step 3: Update as-built docs**

Update docs to say Kiro CLI marker detection, passthrough behavior, and usage extraction are implemented only if Tasks 1 through 5 pass. Keep the sentence that Kiro CLI is distinct from Kiro EventStream and that dispatch uses `ApiShape`, not `ProtocolKind`.

Expected: Docs describe implemented evidence-backed behavior without claiming unsupported sample shapes.

- [ ] **Step 4: Close roadmap only after usage is settled**

Close the roadmap item only if sample-proven usage extraction is non-zero for stream and non-stream success, or a non-zero fallback/settlement decision is documented in Task 1 evidence.

Expected: `docs/ROADMAP.md` no longer lists Kiro CLI detector/usage/passthrough as open only when all stop conditions are cleared.

## Acceptance Commands

Use these lightweight commands during planning and sample review:

```bash
git status --short --branch
rg -n "Kiro CLI|KiroCli|ApiShape::KiroCli|ProtocolKind::Kiro|detect_cli|usage extractor|roadmap" docs crates/gw-codec crates/gw-proxy
rg -n "T[O]DO|T[B]D|implement [l]ater|fill in [d]etails|add [a]ppropriate|Similar to [T]ask|unsupported [g]uess" docs/superpowers/plans/2026-06-10-w9-kiro-cli-sample-contract.md
```

Expected: the first command finds the relevant Kiro CLI boundaries; the second command prints no matches.

After samples and code implementation are available, use focused Rust tests only:

```bash
cargo test -p gw-codec kiro_cli
cargo test -p gw-proxy kiro_cli
```

Expected final acceptance:

- Real redacted Kiro CLI stream, non-stream, and error fixtures exist.
- `detect_cli` returns `ApiShape::KiroCli` only for sample-proven CLI markers.
- Kiro EventStream remains `AnyCodec::Kiro` / `ApiShape::KiroEventStream`.
- Proxy usage extraction for `ApiShape::KiroCli` is no longer noop.
- Stream and non-stream Kiro CLI success paths produce non-zero usage, or a documented non-zero fallback/settlement decision exists.
- Roadmap closure occurs only after the usage stop condition is cleared.
