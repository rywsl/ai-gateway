# WS-8 — Usage Schema & Cross-Process Contract (TDD Sub-Plan)

> Parent plans:
> - v1: `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-8
> - v2: `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §4 WS-8 (REV-FINAL-H8.1..H8.9)
>
> Mirror: `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-8-usage-schema-contract.md` (byte-identical except title).
>
> Coverage: original Codec-2 (audio + accepted/rejected prediction tokens lost), Codec-3 (Gemini ToolCall arguments lost during cross-protocol render), Codec-5 (cache_creation 5m/1h tier + server_tool_use lost), and contract integrity C1, C2, C3, C4, C5, C6 (decline), C7 (handoff to WS-1), C10, C11. The dedup-key piece (C4) and Terminated semantics (C3) are jointly owned with WS-1 T22 and T16/T17 — Go-side edits to `modules/billing/business/dedup.go` MUST land in the same PR as WS-1 T21/T22 to avoid the same-file conflict flagged in v2 REV-003a.
>
> Audience assumption: executor knows neither Rust nor Go fluently. Every step gives a literal file path, the literal snippet to read first, and the literal grep/test invocation to run.

---

## 0. Reading discipline (per v2 §0 重要 1 + 重要 2)

Before touching anything, read the v2 plan §0 + §4 WS-8 in full. The bullets below capture only the WS-8 deltas; they presuppose §0.

The **most important rule**: every "candidate fix" in v2 is a **hypothesis**, not a conclusion. v2 explicitly cites the H9 reversal — the original audit declared `api_keys.deleted_at IS NULL` was missing, but `api_keys` has no `deleted_at` column at all. Before each task, open the cited file at the cited lines and confirm the snippet matches what the v2 hypothesis assumes. If the file disagrees with v2, the file wins; record the divergence in section 7 of the task and pick an alternative.

### Hypotheses already verified by the planning author (do not re-litigate)

| Hypothesis | Verified? | Evidence |
|---|---|---|
| Go `provenanceRank` table is missing `cli_extracted` | YES | `modules/billing/business/dedup.go:18-25` — table is `upstream_final=5, upstream_partial=4, estimated=3, crash_fallback=2, unknown=1`. `cli_extracted` is absent → normalized to `unknown` (rank 1) → silent CLI billing-guard downgrade. |
| Rust IR has `cli_extracted` | YES | `crates/gw-core/src/usage.rs:25-39` defines `ProvenanceTag::CliExtracted` and serializes as `"cli_extracted"`. So Rust IS emitting this tag; Go IS losing it. |
| Go dedup key has no attempt_index | YES | `modules/billing/business/dedup.go:56-60` — `dedupKey`, `settledDoneKey`, `settlingKey`, `legacyDedupKey`, `pendingLegacyKey` all key on `requestID` only. |
| `parts/*` are stale vs Rust HEAD | YES | All four `parts/0X-*.md` carry header `commit: 1674cd14...` synced `2026-06-10`. `git -C ailinkmax-gateway rev-parse HEAD` = `a18a2ab1...`, 2026-06-17. Drift window is 7 days. |
| Rust IR has the 8 missing dimensions; Rust emit type does not | YES | `crates/gw-core/src/ir.rs:337-355` defines `UsageIR` with `audio_tokens_in/out`, `accepted/rejected_prediction_tokens`, `cache_creation_5m/1h_tokens`, `server_tool_web_search/fetch_requests`. `crates/gw-core/src/usage.rs:11-19` defines `UsageSnapshot` with only the 6 base dims. The new fields are extracted by codecs but discarded before reaching Go. |
| No `DisallowUnknownFields` in Go decode path | YES | `grep -rn DisallowUnknownFields shared/rustpact modules/billing` returns nothing; `shared/rustpact/usage.go` is decoded with plain `json.Unmarshal`. v1's S8.3 would have introduced it; v2 H8.1 correctly drops that idea in favor of `_pv`. |

### Hypotheses still to be verified inside individual tasks

- v1 S8.5 claim that Anthropic→Gemini tool-call args are dropped during streaming (Codec-3). Codec author must read `crates/gw-codec/src/gemini/via.rs` `ToolCallDelta` arm before designing.
- v1 S8.9 claim that `modules/billing/model/queries/model_pricing.sql` exists — that exact path does **not** exist; pricing storage is `global_models.default_tiered_pricing` (jsonb) consumed by `modules/model/business/pricing.go`. The pricing-table task in WS-8 must use the real surface. See REV-LOCAL-A below.

---

## 1. Local revisions discovered during planning (record so executor doesn't repeat the work)

| ID | Finding | Action |
|---|---|---|
| **REV-LOCAL-A** | v1 S8.9 cites `modules/billing/model/queries/model_pricing.sql`; that path does not exist. Real pricing storage is JSONB at `global_models.default_tiered_pricing` consumed by `modules/model/business/pricing.go` (`BillingPricing` struct, line 24). The 6 `*Per1M` fields there are the actual surface. | Task W8-T09 targets `modules/model/business/pricing.go` + `shared/contracts/pricing.go` `ChargeInput`/`CostBreakdown`, not a non-existent SQL file. |
| **REV-LOCAL-B** | v2 REV-FINAL-H8.9 reverses v1 ordering — Rust deploys FIRST (it begins emitting `attempt_index` and the new `_pv:2` schema), Go follows. v1 §6.1 said Go first. Both v1 §5.5 C4 and v1 §4 WS-1 S1.20 contradict this. The v2 rule wins. | Task W8-T07 (CI gate) enforces "Rust HEAD `_pv` ≤ Go-supported `_pv` max"; v1's reverse rule is repealed. |
| **REV-LOCAL-C** | C5 (request_id is Rust-allocated) is mostly already true: `crates/gw-proxy/src/filters/request.rs:211` uses `ctx.request_id.0` which is set in `early.rs`. But there is no explicit response-header `X-Request-Id` write. Task W8-T08 adds the response header + the codified policy that any client-supplied `X-Request-Id` is trace-only. | Task W8-T08. |
| **REV-LOCAL-D** | Rust HEAD differs from synced commit (`1674cd14...` → `a18a2ab1...`, 2026-06-10 → 2026-06-17). Sync MUST be re-run as task W8-T01 before any other WS-8 task; its diff must be reviewed for any field the Rust side has added since 2026-06-10 that this plan didn't anticipate. | Task W8-T01 is the precondition for all others. |
| **REV-LOCAL-E** | v1 S8.6 says C4 is covered in WS-1; v2 §5.5 corrected the mapping but the dedup-key changes still need to land in `modules/billing/business/dedup.go` — same file as WS-1 T21/T22. Per REV-003a the two PRs MUST be one PR, owned by Track A1. | Task W8-T06 declares a hard dependency on WS-1 T22 and ships in the same PR. |

---

## 2. Task list overview

| Task | Subject | Files (literal) | Estimated time | Blocks / Blocked-by |
|---|---|---|---|---|
| **W8-T01** | Refresh contract baseline & freeze diff policy | `scripts/sync-rust-contract.sh`, `docs/rust-contract/parts/*`, `.github/workflows/contract-sync.yml` (new) | 0.5d | Blocks all WS-8 tasks |
| **W8-T02** | Extend `UsageSnapshot` (Rust) + `Usage` (Go) with 8 new u64 fields and `_pv` | `crates/gw-core/src/usage.rs`, `crates/gw-ledger/src/usage.rs`, `shared/rustpact/usage.go`, `shared/rustpact/usage_test.go` | 1d | Blocked by T01 |
| **W8-T03** | Wire codec extractors to populate the 8 new fields end-to-end | `crates/gw-codec/src/openai/{wire.rs,via.rs}`, `crates/gw-codec/src/anthropic/{wire.rs,via.rs,codec.rs}`, `crates/gw-codec/src/gemini/wire.rs`, `crates/gw-ledger/src/flusher.rs` | 1d | Blocked by T02 |
| **W8-T04** | Repoint provenance ladder, add `cli_extracted`, version stored value as `v3:rank:cost` | `modules/billing/business/dedup.go`, `modules/billing/business/dedup_test.go` | 1d | Blocked by T02 (shares PR with T06) |
| **W8-T05** | ToolCallEnd as signal event (Codec-3) — emit aggregated args at boundary, not per chunk | `crates/gw-core/src/ir.rs`, `crates/gw-codec/src/anthropic/codec.rs`, `crates/gw-codec/src/gemini/codec.rs`, `crates/gw-codec/src/cross_render.rs` | 1d | Blocked by T03 |
| **W8-T06** | Dedup-key BNF (`settled[:done|:pending]:{rid}[:{attempt}]`) with legacy fallback | `modules/billing/business/dedup.go`, `modules/billing/business/dedup_test.go`, `modules/billing/business/handler.go`, `modules/billing/business/inflight_reaper.go` | 1.5d | Blocked by T02; co-PR with WS-1 T22 |
| **W8-T07** | CI gate: Rust HEAD `_pv` ≤ Go `RustpactMaxSupportedVersion`; rename/remove abort | `shared/rustpact/version.go` (new), `scripts/check-rustpact-pv.sh`, `scripts/check-deploy-order.sh`, `scripts/sync-rust-contract.sh` (additive-only flag) | 0.5d | Blocked by T02 |
| **W8-T08** | `request_id` allocation = Rust-only; `X-Request-Id` response header | `crates/gw-proxy/src/filters/request.rs`, `crates/gw-proxy/src/filters/response.rs`, `docs/rust-contract/parts/02-go-as-consumer.md` | 0.5d | Independent |
| **W8-T09** | `model_pricing` two-phase: nullable new columns + settler NULL-rejection | `shared/contracts/pricing.go`, `modules/model/business/pricing.go`, `modules/billing/business/handler.go`, `modules/billing/business/settler.go`, `migrations/00043_global_models_extended_pricing.sql` (new) | 1d | Blocked by T02 |
| **W8-T10** | Decline `dup_safe` field (C6) and document why | `docs/rust-contract/parts/02-go-as-consumer.md`, `docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` (cross-ref) | 0.2d | Independent |

Total: ~8 person-days, achievable in 4 working days by 2 owners (one Rust-leaning, one Go-leaning).

---

## 3. Tasks (8-section format per v2 §0)

Each task uses the v2 §3 7-section template plus a `[Dependencies / Adversarial review]` 8th section.

---

### TASK W8-T01: Refresh contract baseline & lock the diff-review policy

**【是什么 What it IS】**
- An operations + docs task: re-run `scripts/sync-rust-contract.sh` against the current `ailinkmax-gateway` HEAD (`a18a2ab1a4229c2188704202bebbc7ef9a2dc362`), inspect the resulting diff under `docs/rust-contract/parts/*.md`, and harden the script so subsequent runs cannot accept destructive deltas without a human decision.
- Output artifacts: refreshed `docs/rust-contract/parts/{01,02,03,04,05,06,07,08}-*.md` (headers will show today's date + new commit SHA), one paragraph in the PR description describing every additive field/event, and a new optional flag in `scripts/sync-rust-contract.sh` (`--require-additive`) that exits non-zero when any pre-existing field rename or removal appears in the diff.

**【不是什么 What it is NOT】**
- ❌ Not the place to extend `UsageSnapshot` — that is T02.
- ❌ Not the place to delete obsolete `parts/*` files; the sync script does an `rm -rf` rebuild already.
- ❌ Not a rewrite of the script; only the additive-check is new.
- ❌ Does not touch `ailinkmax-gateway/docs/go-contracts/*` — those are the **source** of truth for this sync; edits there happen in the Rust repo, not here.

**【为什么 Why this exists / why bug occurs】**
- Root cause: contract docs lag the live Rust contract. Synced commit is `1674cd14...` (2026-06-10) but Rust HEAD is `a18a2ab1...` (2026-06-17). Any plan written against the stale docs may miss a field Rust started emitting in the last 7 days (e.g. coverage of `pod_id`, `coef_table_version`, new `attempted_keys`).
- Cited findings: C10 ("parts/* lag Rust main"), C11 ("no protocol version → silent data loss"). v2 REV-FINAL-H8.6 mandates rename/remove abort.

**【做什么 DO】**
1. Run the sync script with the local sibling checkout: `bash scripts/sync-rust-contract.sh`. Capture stdout (it prints the commit it pinned and a unified diff vs the previous snapshot).
2. Read the printed diff top-to-bottom. For every entry, classify as `additive` (new field, new event, new section) or `mutating` (renamed field, removed field, type change). Record the classification in the PR description as a checklist.
3. If any entry is `mutating`, STOP and escalate — that is a contract break that must be handled in the originating Rust PR before WS-8 continues.
4. If everything is additive, edit `scripts/sync-rust-contract.sh` to (a) add a `--require-additive` flag that aborts on any line in the diff matching `^-.*: [A-Z]` or removed `pub struct` / `pub enum` lines, and (b) print the commit SHA prominently to stdout so the operator can paste it into the PR description.
5. Add `.github/workflows/contract-sync.yml` (new) that runs `bash scripts/sync-rust-contract.sh --require-additive` on every PR that touches `shared/rustpact/**` OR `docs/rust-contract/**`. Failure of this check is a merge blocker.
6. Commit the refreshed `parts/*` plus the new flag in the same PR; PR title `chore(contract): sync rust-contract baseline + additive-only gate`.

**【不做什么 DON'T】**
- ❌ Do not manually edit `docs/rust-contract/parts/*.md` — the script overwrites them.
- ❌ Do not skip the `mutating` check by hand-waving "but it's just a rename" — every rename is silent data loss until both sides ship together.
- ❌ Do not lower the diff-review bar to "git diff stats look fine"; read every removed/added line.
- ❌ Do not add `--require-additive` as the default behavior of the script in this PR; default stays unchanged so existing manual runs are unaffected. Only the CI workflow opts in.

**【怎么做 HOW (literal)】**

1. Verify Rust sibling checkout is present and on a sensible commit:
   ```bash
   git -C ../ailinkmax-gateway rev-parse HEAD
   git -C ../ailinkmax-gateway log --oneline -5
   ```
   Expected: `a18a2ab1a4229c2188704202bebbc7ef9a2dc362` or newer; latest commit message names a recent contract change. If the sibling is on an old branch, `git -C ../ailinkmax-gateway checkout main && git pull`.

2. Run the sync:
   ```bash
   bash scripts/sync-rust-contract.sh 2>&1 | tee /tmp/ws8-t01-sync.log
   ```
   Expected last line: `synced rust-contract @ main/a18a2ab1 (2026-06-17)`.

3. Inspect `parts/*` headers and confirm the date moved:
   ```bash
   head -2 docs/rust-contract/parts/*.md
   ```
   Each file should now show `synced: 2026-06-17` and `commit: a18a2ab1...`.

4. Edit `scripts/sync-rust-contract.sh`. After the existing `if [[ -n "$prev" ]]; then` block, add:
   ```bash
   if [[ "${REQUIRE_ADDITIVE:-0}" == "1" ]]; then
     # Mutating change patterns: a removed line that looks like a struct field, an enum variant, or a heading.
     if diff -ru "$prev" "$DEST" | grep -E '^-\s*(pub\s+(struct|enum|fn)\b|[a-zA-Z_][a-zA-Z0-9_]*\s*:)' >/dev/null; then
       echo "ABORT: contract sync produced non-additive changes; review and handle in Rust PR first." >&2
       exit 2
     fi
   fi
   ```
   And at the top of the script add a flag parser:
   ```bash
   for arg in "$@"; do
     case "$arg" in
       --require-additive) REQUIRE_ADDITIVE=1 ;;
       *) echo "unknown arg: $arg" >&2; exit 64 ;;
     esac
   done
   ```

5. Mirror the same edits to `scripts/sync-rust-contract.ps1` (two-line search/replace — see the PowerShell file's existing structure; the conditional becomes `if ($env:REQUIRE_ADDITIVE -eq '1')` with a `Select-String -Pattern '^-\s*(pub\s+(struct|enum|fn)\b|[a-zA-Z_][a-zA-Z0-9_]*\s*:)'` check on `git diff` output). Two-shell parity is required by CLAUDE.md's last sync rule.

6. Create `.github/workflows/contract-sync.yml`:
   ```yaml
   name: contract-sync-additive
   on:
     pull_request:
       paths:
         - 'shared/rustpact/**'
         - 'docs/rust-contract/**'
   jobs:
     additive-only:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
           with: { submodules: false, fetch-depth: 0 }
         - name: clone Rust sibling
           run: git clone --depth 1 https://github.com/cnxx-tech/AILinkMax-Gateway.git ../ailinkmax-gateway
         - name: sync + additive gate
           run: REQUIRE_ADDITIVE=1 bash scripts/sync-rust-contract.sh
   ```

**【如何验证 VERIFY】**

1. Local sync produces the expected baseline:
   ```bash
   bash scripts/sync-rust-contract.sh
   grep -l "synced: 2026-06-17" docs/rust-contract/parts/*.md | wc -l
   ```
   Expected: `8` (all eight parts updated).
2. Additive-only gate trips on a synthetic rename:
   ```bash
   # rehearsal: pretend a field was renamed in the prev snapshot
   sed -i.bak 's/pub request_id: String/pub req_id: String/' docs/rust-contract/parts/02-go-as-consumer.md
   REQUIRE_ADDITIVE=1 bash scripts/sync-rust-contract.sh; echo "exit=$?"
   ```
   Expected: `exit=2` and a message containing `ABORT: contract sync produced non-additive changes`. Restore the file from the `.bak`.
3. Additive-only gate passes on a clean run:
   ```bash
   REQUIRE_ADDITIVE=1 bash scripts/sync-rust-contract.sh; echo "exit=$?"
   ```
   Expected: `exit=0`.
4. CI check appears on the PR diff (GitHub Actions UI): a passing `contract-sync-additive` status.

**【Dependencies / Adversarial review】**
- Blocks T02..T09. T01 must merge first.
- Adversarial review prompt for sonnet: "Read the resulting diff of `parts/*.md` and assert: does any field shrink, rename, or change type? Cite line numbers."

---

### TASK W8-T02: Extend `UsageSnapshot` and `Usage` with 8 new fields + `_pv`

**【是什么 What it IS】**
- A code change on both sides of the cross-process contract. On Rust, extend `crates/gw-core/src/usage.rs` `UsageSnapshot` from 6 fields to 14 fields (existing 6 stay; 8 new `u64`). On Go, mirror the additions in `shared/rustpact/usage.go` `Usage` struct (matching `int64`).
- Both sides gain a `_pv u32` (Rust) / `int32` (Go) versioning byte: emit `_pv: 2`. Old records (no `_pv` key) are treated as `_pv: 1`.
- Two emit paths get touched: `crates/gw-ledger/src/usage.rs` (the `UsageRecord` struct that wraps `UsageSnapshot`) — confirm no defensive narrowing happens between codec and ledger.

**【不是什么 What it is NOT】**
- ❌ Does NOT wire up codec extractors to actually populate the new fields — that's T03.
- ❌ Does NOT add `serde(deny_unknown_fields)` on either side. Per v2 REV-FINAL-H8.1 this is forbidden; we use `_pv` instead.
- ❌ Does NOT change the existing 6 fields' types (`u32` stays `u32` on Rust, `int64` stays `int64` on Go) in this task. v2 H8.3 calls for upgrading them to `u64`; we defer that to a follow-up because it requires a careful bench on hot-path serialization, and is not blocking the missing-field bug.
- ❌ Does NOT touch `parts/*.md` — those will be regenerated by the Rust-side PR running the sync script (which is upstream of T01).

**【为什么 Why this exists / why bug occurs】**
- Root cause: Rust's `UsageIR` (in `crates/gw-core/src/ir.rs:337-355`) carries 13+ dimensions including audio, prediction, cache-tier, and server-tool counters. The emit type `UsageSnapshot` (in `crates/gw-core/src/usage.rs:11-19`) carries only 6. The 8 missing dimensions are extracted by codecs and silently dropped at the IR→Snapshot transition. The same gap exists on the Go consumer side (`shared/rustpact/usage.go:59-66` mirrors only 6 fields).
- Cited findings: Codec-2 (audio + prediction lost), Codec-5 (cache_creation tier + server_tool_use lost), C1 ("8 new fields in IR but not in Snapshot"), C11 ("no protocol version → silent data loss").

**【做什么 DO】**
1. On Rust, extend `UsageSnapshot` with 8 new fields, all `Option<u64>` with `#[serde(skip_serializing_if = "Option::is_none")]`. Why `Option`: a `0` is meaningfully different from "vendor didn't report this dimension" for billing (a billed-but-zero audio call needs a `Some(0)`; a non-audio model needs `None`).
2. Add `_pv: u32` (with `#[serde(rename = "_pv", default = "default_pv")] pub _pv: u32`) and `fn default_pv() -> u32 { 1 }`. Construct with `_pv: 2`.
3. On Go, add the 8 fields to `shared/rustpact/usage.go` `Usage` struct with matching JSON tags + `,omitempty`, type `*int64` (pointer to disambiguate zero from absent), and a `PV int32 ` + tag ` ` + ` `_pv,omitempty` `.
4. Provide a `ProtocolVersion()` method on `Usage` returning the `PV` (defaulting to 1 if zero/absent).
5. Add a single round-trip test on Go covering: (a) `_pv: 2` record with all 8 new fields present, (b) `_pv` absent (legacy record) round-trips as `PV()==1`, (c) unknown future field is ignored (no error).
6. Add a `_pv` boundary test on Go: if a record arrives with `_pv: 99` (i.e. higher than `MaxSupportedPV = 2`), `Usage.Validate()` returns `ErrUnsupportedProtocolVersion` so the handler can DLQ.

**【不做什么 DON'T】**
- ❌ Do NOT introduce `json.NewDecoder(...).DisallowUnknownFields()` anywhere. This is the v1 mistake that v2 H8.1 reverses; doing so would DLQ every legitimate forward-compatible Rust addition.
- ❌ Do NOT make the 8 new fields non-Optional. A required field would mean older Rust binaries (still on `_pv: 1`) emit JSON that fails Go's `_pv: 2` schema.
- ❌ Do NOT bump `_pv` to anything other than `2`. v2 H8.2 reserves `3` for the storage-value rewrite in T04 + T06.
- ❌ Do NOT add the 8 fields to `crates/gw-ledger/src/usage.rs` `UsageRecord` directly — they ride inside the nested `usage: UsageSnapshot`. Touching `UsageRecord` flattens them and confuses Go.

**【怎么做 HOW (literal)】**

1. Read first to confirm the current shape:
   ```bash
   sed -n '10,20p' /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-core/src/usage.rs
   sed -n '55,75p' /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/rustpact/usage.go
   ```
   Confirm Rust has exactly 6 fields and Go has exactly 6 fields. If either has drift since this plan was written, stop and reconcile.

2. Patch `crates/gw-core/src/usage.rs` — append 8 fields before the closing brace at line 19, then add `_pv`:
   ```rust
   #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
   pub struct UsageSnapshot {
       pub input_tokens: u32,
       pub output_tokens: u32,
       pub cache_read_input_tokens: Option<u32>,
       pub cache_creation_input_tokens: Option<u32>,
       pub thinking_tokens: Option<u32>,
       pub multimodal_input_tokens: Option<u32>,

       // --- C1 + Codec-2 + Codec-5: audio, prediction, cache-creation tier, server-tool counts ---
       #[serde(skip_serializing_if = "Option::is_none")]
       pub audio_input_tokens: Option<u64>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub audio_output_tokens: Option<u64>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub accepted_prediction_tokens: Option<u64>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub rejected_prediction_tokens: Option<u64>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub cache_creation_5m_tokens: Option<u64>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub cache_creation_1h_tokens: Option<u64>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub server_tool_web_search_count: Option<u64>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub server_tool_web_fetch_count: Option<u64>,

       // --- C11: protocol version. Older records default to 1. New emit writes 2. ---
       #[serde(rename = "_pv", default = "default_pv")]
       pub pv: u32,
   }

   fn default_pv() -> u32 { 1 }

   impl UsageSnapshot {
       pub const CURRENT_PV: u32 = 2;
   }
   ```
   Then update the `Default` derive site — `Default` for `Option<T>` is `None` and for `u32` is `0`, but we want `pv = 1` on `Default::default()` (so existing test fixtures continue to compile as "legacy"). Replace `#[derive(... Default ...)]` with a hand-written `Default`:
   ```rust
   impl Default for UsageSnapshot {
       fn default() -> Self {
           Self {
               input_tokens: 0, output_tokens: 0,
               cache_read_input_tokens: None, cache_creation_input_tokens: None,
               thinking_tokens: None, multimodal_input_tokens: None,
               audio_input_tokens: None, audio_output_tokens: None,
               accepted_prediction_tokens: None, rejected_prediction_tokens: None,
               cache_creation_5m_tokens: None, cache_creation_1h_tokens: None,
               server_tool_web_search_count: None, server_tool_web_fetch_count: None,
               pv: 1,
           }
       }
   }
   ```
   And drop the `Default` derive from the struct attribute list.

3. Confirm `crates/gw-ledger/src/usage.rs` does NOT redefine any of these fields. `grep -n "audio_input_tokens\|cache_creation_5m" crates/gw-ledger/src/usage.rs` should be empty.

4. Patch `shared/rustpact/usage.go` — extend the `Usage` struct (line 59-66 in current file):
   ```go
   type Usage struct {
       InputTokens              int64 `json:"input_tokens"`
       OutputTokens             int64 `json:"output_tokens"`
       CacheReadInputTokens     int64 `json:"cache_read_input_tokens,omitempty"`
       CacheCreationInputTokens int64 `json:"cache_creation_input_tokens,omitempty"`
       ThinkingTokens           int64 `json:"thinking_tokens,omitempty"`
       MultimodalInputTokens    int64 `json:"multimodal_input_tokens,omitempty"`

       // C1 + Codec-2 + Codec-5. Pointer so a billed-but-zero audio call is
       // distinguishable from a non-audio model (where the field is absent).
       AudioInputTokens            *int64 `json:"audio_input_tokens,omitempty"`
       AudioOutputTokens           *int64 `json:"audio_output_tokens,omitempty"`
       AcceptedPredictionTokens    *int64 `json:"accepted_prediction_tokens,omitempty"`
       RejectedPredictionTokens    *int64 `json:"rejected_prediction_tokens,omitempty"`
       CacheCreation5mTokens       *int64 `json:"cache_creation_5m_tokens,omitempty"`
       CacheCreation1hTokens       *int64 `json:"cache_creation_1h_tokens,omitempty"`
       ServerToolWebSearchCount    *int64 `json:"server_tool_web_search_count,omitempty"`
       ServerToolWebFetchCount     *int64 `json:"server_tool_web_fetch_count,omitempty"`

       PV int32 `json:"_pv,omitempty"` // C11; 0 == absent (legacy) → treated as 1.
   }

   // MaxSupportedPV is the highest _pv this Go binary can decode. Bump in lockstep
   // with Rust emit; see scripts/check-rustpact-pv.sh and W8-T07.
   const MaxSupportedPV int32 = 2

   var ErrUnsupportedProtocolVersion = errors.New("rustpact: usage._pv beyond MaxSupportedPV")

   func (u Usage) ProtocolVersion() int32 {
       if u.PV == 0 {
           return 1
       }
       return u.PV
   }

   func (u Usage) Validate() error {
       if u.PV > MaxSupportedPV {
           return ErrUnsupportedProtocolVersion
       }
       return nil
   }
   ```
   Add `errors` to the import block at the top of `usage.go` if not already present.

5. Add `shared/rustpact/usage_test.go` cases (append at bottom):
   ```go
   func TestUsage_PV_DefaultIsOne(t *testing.T) {
       var u Usage
       require.NoError(t, json.Unmarshal([]byte(`{"input_tokens":10,"output_tokens":5}`), &u))
       require.Equal(t, int32(1), u.ProtocolVersion())
       require.NoError(t, u.Validate())
   }
   func TestUsage_PV2_WithNewFields(t *testing.T) {
       body := `{"input_tokens":10,"output_tokens":5,"audio_input_tokens":3,"_pv":2}`
       var u Usage
       require.NoError(t, json.Unmarshal([]byte(body), &u))
       require.Equal(t, int32(2), u.ProtocolVersion())
       require.NotNil(t, u.AudioInputTokens)
       require.Equal(t, int64(3), *u.AudioInputTokens)
       require.NoError(t, u.Validate())
   }
   func TestUsage_PV_BeyondSupported_IsDLQ(t *testing.T) {
       var u Usage
       require.NoError(t, json.Unmarshal([]byte(`{"_pv":99}`), &u))
       require.ErrorIs(t, u.Validate(), ErrUnsupportedProtocolVersion)
   }
   func TestUsage_UnknownField_IsIgnored(t *testing.T) {
       // confirm we did NOT enable DisallowUnknownFields.
       var u Usage
       require.NoError(t, json.Unmarshal([]byte(`{"input_tokens":1,"some_future_field":"x"}`), &u))
       require.Equal(t, int64(1), u.InputTokens)
   }
   ```

**【如何验证 VERIFY】**
1. Rust side:
   ```bash
   cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
   cargo test -p gw-core --lib usage::tests
   cargo test -p gw-ledger --lib usage::tests
   ```
   Expected: both green; serde round-trip preserves `Some(3)` for audio_input_tokens; default constructor produces `pv == 1`.
2. Go side:
   ```bash
   cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
   go test -race ./shared/rustpact/ -run TestUsage -count=5
   ```
   Expected: all 4 new tests PASS five times in a row under `-race`.
3. Confirm no `DisallowUnknownFields` was introduced:
   ```bash
   grep -r "DisallowUnknownFields" shared/ modules/billing/business/
   ```
   Expected: empty output.

**【Dependencies / Adversarial review】**
- Blocked by T01.
- Blocks T03, T04, T06, T07, T09.
- Adversarial review: ask sonnet to produce a record JSON that "looks like a future Rust version" (e.g. `{"_pv": 3, "input_tokens": 1, "tax_id": "abc"}`) and confirm Go behavior: it must Validate() → ErrUnsupportedProtocolVersion (because `3 > MaxSupportedPV=2`); confirm there is no panic or silent data loss. Also ask sonnet to design an attack where the field name collides with a Go reserved word; confirm rejected.

---

### TASK W8-T03: Wire codec extractors to populate the 8 new fields

**【是什么 What it IS】**
- A Rust-only change inside `crates/gw-codec/src/{openai,anthropic,gemini}/`. After T02 created the field slots on `UsageSnapshot`, this task makes the codecs actually copy the values from `UsageIR` (which already has them) into `UsageSnapshot` at the emit boundary.
- Touch points: `usage_extractor.rs` (per-codec extractor returning `ExtractedUsage`) and the `via.rs` / `wire.rs` that map vendor JSON ⇄ IR. The flusher in `crates/gw-ledger/src/flusher.rs` is the only place that converts `UsageIR` → `UsageSnapshot`; that conversion needs the 8 new field copies.

**【不是什么 What it is NOT】**
- ❌ Does NOT change `UsageIR` itself — those 13 fields already exist (see `crates/gw-core/src/ir.rs:337-355`).
- ❌ Does NOT touch cross-protocol re-render rules (codec-3 ToolCallEnd handling is T05).
- ❌ Does NOT touch billing: Go-side pricing-table extension for the 8 new dims is T09.

**【为什么 Why this exists / why bug occurs】**
- Root cause: the IR→Snapshot copy is narrow. Audio/prediction/cache-tier fields are extracted into `UsageIR` (e.g. `crates/gw-codec/src/openai/via.rs:709` already sets `accepted_prediction_tokens`), but the conversion step that produces `UsageSnapshot` for the `usage:records` stream silently drops them.
- Cited findings: Codec-2 (audio + prediction), Codec-5 (cache_creation tier + server_tool_use).

**【做什么 DO】**
1. Locate the single function that converts `UsageIR → UsageSnapshot`. Run `rg "UsageIR" crates/gw-ledger/ crates/gw-codec/` and find the `From<UsageIR>` impl or the explicit constructor — usually in `crates/gw-ledger/src/flusher.rs` near the `XADD` site. Read it.
2. Add field-by-field copies for the 8 new dims, mapping `Option<u32>` (IR) → `Option<u64>` (Snapshot) via `.map(u64::from)`.
3. Set `pv: UsageSnapshot::CURRENT_PV` (`= 2`) at the construction site.
4. Trace each vendor codec's existing extraction path for the 8 new fields to confirm IR carries the value (e.g. `crates/gw-codec/src/anthropic/codec.rs:396-399` for cache_creation_1h/5m). If any vendor codec doesn't yet populate IR for one of the 8, file a TODO comment referencing this plan and leave that field `None` — that gap is a Codec-2/3/5 follow-up, not a WS-8 blocker.

**【不做什么 DON'T】**
- ❌ Do NOT widen `UsageIR` field types — keep `u32`.
- ❌ Do NOT mutate codec hot-path allocations (no new `String::from`, no `to_owned`) — the IR→Snapshot copy is per-request and a `u32→u64` cast is free.
- ❌ Do NOT add the 8 fields to `UsageRecord` (the outer struct) — they ride inside `usage: UsageSnapshot`.

**【怎么做 HOW (literal)】**
1. Find the conversion:
   ```bash
   cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
   rg "From<UsageIR>|UsageIR.*->.*UsageSnapshot|fn snapshot|fn to_snapshot" crates/gw-codec crates/gw-ledger
   ```
2. In the conversion function, replace existing 6-field copy with 14-field copy + `pv: UsageSnapshot::CURRENT_PV`. Use `usage_ir.audio_tokens_in.map(u64::from)` style for each new field. Mapping table (IR → Snapshot):

   | UsageIR field | UsageSnapshot field |
   |---|---|
   | `audio_tokens_in` | `audio_input_tokens` |
   | `audio_tokens_out` | `audio_output_tokens` |
   | `accepted_prediction_tokens` | `accepted_prediction_tokens` |
   | `rejected_prediction_tokens` | `rejected_prediction_tokens` |
   | `cache_creation_5m_tokens` | `cache_creation_5m_tokens` |
   | `cache_creation_1h_tokens` | `cache_creation_1h_tokens` |
   | `server_tool_web_search_requests` | `server_tool_web_search_count` |
   | `server_tool_web_fetch_requests` | `server_tool_web_fetch_count` |

3. Add a regression test in `crates/gw-ledger/src/usage.rs#tests` that builds a `UsageIR` with all 8 new fields populated, runs the conversion, and asserts each is `Some(expected)` on the resulting Snapshot.

**【如何验证 VERIFY】**
1. ```bash
   cargo test -p gw-ledger --lib usage::tests
   cargo test -p gw-codec --workspace
   ```
   Expected green.
2. Soak smoke (per `ailinkmax-gateway/CLAUDE.md`):
   ```bash
   cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
   make e2e-soak-smoke 2>&1 | tail -30
   ```
   Expected: no new failures; new fields appear in `tests/e2e/client/.soak_report/` JSON for any request that exercises audio / prediction / cache tier.
3. Spot-check end-to-end with the gpt-4o-audio-preview path: pick the soak harness's audio scenario and grep its emitted `usage:records` entry for `audio_input_tokens`.

**【Dependencies / Adversarial review】**
- Blocked by T02.
- Blocks T05 (which assumes new fields exist when designing ToolCallEnd semantics).
- Adversarial review: sonnet must verify that the IR→Snapshot conversion does not double-count when a vendor sends both `cache_creation_input_tokens` (the legacy aggregate) and `cache_creation_5m_tokens` + `cache_creation_1h_tokens` (the tier breakdown). The two MUST NOT both be populated by the same extraction event; if they are, the Snapshot should drop the aggregate and keep the tiered values.

---

### TASK W8-T04: Repoint provenance ladder, add `cli_extracted`, version stored value

**【是什么 What it IS】**
- A Go-side surgical edit to `modules/billing/business/dedup.go`. Two concrete changes:
  1. Insert `cli_extracted` into the `provenanceRank` table at rank `8`, and renumber existing ranks per v2 REV-FINAL-H8.2 to leave gaps: `upstream_final=10, cli_extracted=8, upstream_partial=6, estimated=4, crash_fallback=2, unknown=1`.
  2. Bump the stored Redis value prefix from `v2:` to `v3:`. New writes use `v3:<rank_csv>:<cost>`; reads accept both `v2:` and `v3:` for the migration window (≥ 25h, > the 24h dedup TTL).

**【不是什么 What it is NOT】**
- ❌ Does NOT touch `dedupKey`/`settledDoneKey`/`settlingKey` string composition — that's T06.
- ❌ Does NOT remove `v2:` parsing — the script must keep parsing `v2:` for at least one TTL cycle (24h+) so in-flight records aren't lost during deploy.
- ❌ Does NOT add `cli_extracted` mid-ladder at rank 4 the v1 way; v2 H8.2 spreads ranks 1–10 with gaps so future inserts don't require another renumber.

**【为什么 Why this exists / why bug occurs】**
- Verified root cause: `modules/billing/business/dedup.go:18-25` maps `upstream_final=5 ... unknown=1`. `cli_extracted` is absent. Rust DOES emit `"cli_extracted"` (verified at `crates/gw-core/src/usage.rs:25-39` `ProvenanceTag::CliExtracted` with snake_case rename). When Rust sends a provenance map containing `cli_extracted`, Go's `normalizeProvenanceTag` (line 268-276) does not find it in `provenanceRank`, returns `"unknown"`, and downgrades the CLI-extracted record to rank 1 — silently failing the CLI billing-guard the original Codec-2 was meant to enforce. This is C2.
- Cited findings: C2 (production-live), Codec-2 (CLI extraction not honored downstream).

**【做什么 DO】**
1. Replace the `provenanceRank` map with the new 10/8/6/4/2/1 table; add `cli_extracted=8`.
2. Bump `rankVectorArg` prefix from `"v2:"` to `"v3:"`.
3. In the Lua script `claimCostScript`, add a `v3:` parse branch (mirror the existing `v2:` branch); keep `v2:` and legacy branches intact.
4. Add a Go-side comment: "Migration: v3 writes start at deploy time. v2 records expire within 24h TTL. After 26h, v2 parse branch may be removed in a follow-up PR."
5. Add a unit test that asserts: (a) `cli_extracted` normalizes to rank 8; (b) a Redis value of `v2:1,5,0,0,0,0:1234` and `v3:1,8,0,0,0,0:1234` both parse, with the v3 one winning on upgrade comparison; (c) `unknown` still maps to rank 1.
6. Run `dedup_test.go` end-to-end against a real `miniredis` (already used in `dedup_test.go`).

**【不做什么 DON'T】**
- ❌ Do NOT delete the `s:` or bare-numeric legacy parse branches in `claimCostScript`. They're load-bearing for older Redis residue.
- ❌ Do NOT set `cli_extracted` rank above `upstream_final`. CLI extraction is less authoritative than a true upstream-final usage event.
- ❌ Do NOT change `provenanceDimensions` (it's the dimension vector, not the provenance vector).
- ❌ Do NOT inline the prefix string; keep it `const provenanceValuePrefix = "v3:"` so T06 and future bumps are one-line changes.

**【怎么做 HOW (literal)】**

1. Read `modules/billing/business/dedup.go:18-25` and `:308-321` to confirm baseline:
   ```bash
   sed -n '15,35p;305,325p' /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/billing/business/dedup.go
   ```

2. Edit `provenanceRank`:
   ```go
   // provenance 排序(高→低)。
   // 10/8/6/4/2/1 with gaps so future inserts (e.g. tunnel_relay_extracted = 7)
   // do not force another renumber + Redis storage rewrite.
   // 见 v2 REV-FINAL-H8.2 + C2 (cli_extracted gap fix).
   var provenanceRank = map[string]int{
       "upstream_final":   10,
       "cli_extracted":    8,
       "upstream_partial": 6,
       "estimated":        4,
       "crash_fallback":   2,
       "unknown":          1,
       "":                 0,
   }

   const provenanceValuePrefix = "v3:" // bump in lockstep with claimCostScript writes.
   ```

3. Edit `rankVectorArg` (line ~308):
   ```go
   func rankVectorArg(ranks []int) string {
       parts := make([]string, len(provenanceDimensions))
       for i := range parts {
           var rank int
           if i < len(ranks) {
               rank = ranks[i]
           }
           if rank < 0 {
               rank = 0
           }
           parts[i] = strconv.Itoa(rank)
       }
       return provenanceValuePrefix + strings.Join(parts, ",")
   }
   ```

4. Edit `claimCostScript` `parse_stored` to add a `v3:` branch above the `v2:` branch:
   ```lua
   if string.sub(raw, 1, 3) == 'v3:' then
     local sep = string.find(raw, ':', 4)
     if sep then
       return split_ranks(string.sub(raw, 4, sep - 1)), tonumber(string.sub(raw, sep + 1)) or 0
     end
     return split_ranks(string.sub(raw, 4)), 0
   end
   ```
   Same change to `parse_rank_arg`'s prefix branch. Update the two `redis.call('SET', KEYS[1], 'v2:' .. ...)` writes to `'v3:' .. ...`.

5. Add test in `modules/billing/business/dedup_test.go`:
   ```go
   func TestProvenanceRank_CliExtractedHasRank8(t *testing.T) {
       require.Equal(t, 8, provenanceRank["cli_extracted"])
       require.Equal(t, "cli_extracted", normalizeProvenanceTag("cli_extracted"))
       require.Equal(t, 8, normalizeProvenanceRank("cli_extracted"))
   }
   func TestClaimCost_V3WritesAndReadsBothV2V3(t *testing.T) {
       // Seed Redis with a v2 entry, then write a v3 upgrade and assert delta math.
       // Use existing miniredis harness; see TestClaim_HigherProvenanceUpgrade.
       // ... omitted boilerplate; mirror existing test setup ...
   }
   ```

**【如何验证 VERIFY】**
1. ```bash
   cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
   go test -race ./modules/billing/business/ -run "TestProvenanceRank|TestClaim" -count=10
   ```
   Expected: all pass 10x under `-race`.
2. ```bash
   go test -race ./modules/billing/business/ -run TestDedup -count=3
   ```
   Expected: existing dedup tests all still pass (regression guard for the `v2:` parse branch).
3. Manual: open `dedup.go`, confirm exactly one occurrence of `provenanceValuePrefix` and that all script writes use it.

**【Dependencies / Adversarial review】**
- Blocked by T02.
- Co-PR with T06 (same file). Both must land in the same PR as WS-1 T22 per v2 REV-003a.
- Adversarial review: ask sonnet to construct a Redis state where the same `request_id` has a `v2:` entry with rank `5` (old `upstream_final`) and a new `v3:` write tries rank `10` (new `upstream_final`). The new write must be treated as an upgrade, not a downgrade. Mismatch in rank-scale interpretation between old and new schemes is the most likely silent-failure mode.

---

### TASK W8-T05: ToolCallEnd as signal event (Codec-3)

**【是什么 What it IS】**
- A Rust-side codec change that fixes the Codec-3 finding: cross-protocol tool-call argument loss. Add a new IR event `StreamEventIR::ToolCallEnd { index, valid_json: bool }` and rewire `ToolCallDelta` accumulation so the codec emits one fully-assembled tool-call (or one `valid_json: false` signal) at the end, rather than streaming partial JSON chunks downstream that the receiving codec can't reassemble.

**【不是什么 What it is NOT】**
- ❌ Does NOT change `ToolCallStart` or `ToolCallDelta` semantics for **same-protocol** passthrough — Anthropic→Anthropic still streams deltas verbatim.
- ❌ Does NOT add per-character JSON parsing in the hot path. Validation happens once, at `ToolCallEnd`.
- ❌ Does NOT alter `UsageSnapshot` (Codec-3 is about content, not usage).

**【为什么 Why this exists / why bug occurs】**
- Cited finding: Codec-3 (Gemini ToolCall args lost during cross-protocol render). The IR currently has only `ToolCallStart` + `ToolCallDelta`; cross-protocol render to Gemini needs the full args JSON to call `function_call`, but receiving each chunk in `Cow` form without an end marker means the cross-renderer can either (a) buffer indefinitely or (b) emit on every delta and produce invalid JSON. Both have been observed.

**【做什么 DO】**
1. Hypothesis-check: read `crates/gw-core/src/ir.rs` around line 379 (`ToolCallDelta`). Confirm there is NO existing `ToolCallEnd` event (planning author's grep confirms this). If the file disagrees with this hypothesis, stop and pick a different design from the alternatives below.
2. Add `ToolCallEnd { index: u32, valid_json: bool }` to `StreamEventIR`.
3. Anthropic codec (`crates/gw-codec/src/anthropic/codec.rs`): on `content_block_stop` for a `tool_use` block, parse the accumulated args buffer with `serde_json::from_str::<serde_json::Value>(buf)`; emit `ToolCallEnd { index, valid_json: parse.is_ok() }`.
4. Gemini codec: equivalent on the upstream end-of-tool-call signal.
5. Cross-render (`crates/gw-codec/src/cross_render.rs`): on `ToolCallEnd { valid_json: true }`, flush the accumulated args as a single `function_call` to the target protocol. On `valid_json: false`, emit a `LossyField::drop(from, to, "tool_call.arguments[malformed]")` warning + drop the tool-call.

**Alternative considered + rejected**: ADR-form per v2 §0 重要 2.
  - **Alt-A (chosen)**: New `ToolCallEnd` event with `valid_json` boolean. Pro: explicit, single-allocation per tool-call. Con: every cross-codec must handle the new event.
  - **Alt-B**: Tag the last `ToolCallDelta` with `is_last: true`. Pro: no new event. Con: requires lookahead in the streaming parser, which the current Anthropic codec doesn't do; brittle.
  - **Alt-C**: Buffer the entire stream and emit only on `Stream End`. Pro: simplest. Con: defeats streaming; clients expect to see tool-calls as they arrive in same-protocol passthrough.
  - **Decision**: Alt-A. Buffering happens internally in the codec, not in the IR; same-protocol passthrough still gets per-delta visibility; cross-protocol gets the boundary it needs.

**【不做什么 DON'T】**
- ❌ Do NOT emit `ToolCallEnd` if no `ToolCallStart` preceded it (defensive: protect downstream from spurious end events on malformed streams).
- ❌ Do NOT make `valid_json` an `Option<bool>` — three-state semantics confuse cross-render.
- ❌ Do NOT parse args JSON twice. Do it once at end-of-block and cache the parsed `Value` for the cross-render path.

**【怎么做 HOW (literal)】**
1. ```bash
   grep -n "ToolCallDelta\|content_block_stop" /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-codec/src/anthropic/codec.rs | head -20
   ```
2. Edit `crates/gw-core/src/ir.rs` to add the variant:
   ```rust
   /// Tool-call assembly complete. Cross-render codecs use this signal to flush
   /// the accumulated arguments JSON as a single function_call. valid_json=false
   /// means the upstream produced malformed args; cross-render must drop the call.
   ToolCallEnd {
       index: u32,
       valid_json: bool,
   },
   ```
3. In each codec, locate the existing `ToolCallDelta` accumulator. After the existing buffer-append, add a parse + emit on the block-stop event:
   ```rust
   match serde_json::from_str::<serde_json::Value>(&accumulated[idx]) {
       Ok(_)  => StreamEventIR::ToolCallEnd { index: idx, valid_json: true },
       Err(_) => StreamEventIR::ToolCallEnd { index: idx, valid_json: false },
   }
   ```
4. Update `cross_render.rs` rule table to consume `ToolCallEnd` and emit a single `function_call` to the target wire format. For Anthropic→OpenAI and Anthropic→Gemini specifically.
5. Add a test in `crates/gw-codec/tests/` that runs a fixture of an Anthropic streaming response with one tool-call across three deltas and asserts:
   - 3 `ToolCallDelta` events visible in same-protocol passthrough.
   - 1 `ToolCallEnd { valid_json: true }` at the boundary.
   - Cross-render to Gemini produces exactly one `functionCall` JSON object whose arguments equal the concatenated deltas.

**【如何验证 VERIFY】**
1. ```bash
   cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
   cargo test -p gw-codec tool_call_end
   ```
   Expected: new tests pass.
2. ```bash
   make e2e-soak-smoke 2>&1 | grep -i "tool_call\|cross_render"
   ```
   Expected: no `lossy.*tool_call.arguments` lines for the Anthropic→Gemini cross-render scenario.
3. Manual: pick the soak run's largest single tool-call invocation and confirm `gw_codec_cross_render_lossy_total{field="tool_call.arguments"}` is 0.

**【Dependencies / Adversarial review】**
- Blocked by T03 (the new fields landing first reduces merge friction in `usage_extractor.rs`).
- Adversarial review: sonnet must construct a streaming response where the tool-call is **never closed** (upstream disconnects mid-args). Confirm we still emit `ToolCallEnd { valid_json: false }` from the upstream-disconnect path (in the request-end / `cancel_grace_expired` handler), not just from the happy `content_block_stop`. If the codec doesn't emit ToolCallEnd on disconnect, cross-render will hang forever.

---

### TASK W8-T06: Dedup-key BNF (`settled[:done|:pending]:{rid}[:{attempt}]`) + legacy fallback

**【是什么 What it IS】**
- Extend the four key-builder helpers in `modules/billing/business/dedup.go` to take an `attempt_index int32` parameter and produce the four-variant BNF below, while keeping the legacy single-segment form behind a `LegacyKey()` for the migration window.
- BNF (formal):
  ```
  dedup-key       = "settled" [":" qualifier] ":" rid [":" attempt]
  qualifier       = "done" / "pending"
  rid             = 1*( ALPHA / DIGIT / "-" / "_" )
  attempt         = 1*DIGIT     ; zero-padded form NOT used; "0", "1", ...
  ```
  Live shapes:
  - `settled:done:<rid>:<attempt>`   ← post-success marker (was `settled:done:<rid>`)
  - `settled:pending:<rid>:<attempt>` ← pre-success marker (was `settled:pending:<rid>`)
  - `settling:<rid>:<attempt>`        ← in-progress claim (was `settling:<rid>`)
  - `settled:<rid>:<attempt>`         ← legacy provenance/cost storage value key (was `settled:<rid>`)
- Plus the SAME tasks as WS-1 T22's "Go dedup with attempt_index" — this task is the WS-8 view of that work, co-PR'd with T04 above and WS-1 T22.

**【不是什么 What it is NOT】**
- ❌ Does NOT change the Lua scripts' key argument list shape (they still take 4 keys); only the strings change.
- ❌ Does NOT add a new Redis namespace; the prefix `settled` / `settling` stays.
- ❌ Does NOT touch the `Dedup` struct's external method signatures except to add an `attempt int32` parameter; callers in `handler.go` and `inflight_reaper.go` must be updated in this same PR.
- ❌ Does NOT change the dedup TTL.

**【为什么 Why this exists / why bug occurs】**
- Verified root cause: `modules/billing/business/dedup.go:56-60` keys on `requestID` only. When the same `request_id` retries (attempt 1, attempt 2, …), the second attempt's settle is treated as a duplicate of the first → real billing is masked by an earlier failed attempt. v1 §2.4 C4 calls this out as "production has happened."
- Cited findings: C4 (production-live), v1 T22, v2 REV-FINAL-H8.7.

**【做什么 DO】**
1. Read first: `modules/billing/business/dedup.go:56-92` to confirm current key shape and the four-key Lua call.
2. Change every `func dedupKey(rid string) string` signature to `func dedupKey(rid string, attempt int32) string` (and the three siblings). Append `":" + strconv.Itoa(int(attempt))` to the existing return string.
3. Add legacy fallback: keep a private `func legacyAttemptlessKey(builder func(string) string, rid string) string` for one TTL cycle. The `TryStartSettle` Lua already reads `legacy_settled_key`; extend the script (or wrap the call) so that for the migration window, both the new key shape AND the legacy shape are checked. After the 26h window, drop the legacy probe in a follow-up.
4. Update every caller in `modules/billing/business/handler.go` and `modules/billing/business/inflight_reaper.go` to pass `rec.AttemptIndex` (the field already exists on `shared/rustpact/usage.go:27`).
5. Add tests for the four shapes + a legacy-fallback test that seeds a `settled:rid-1` key (no attempt) and confirms `TryStartSettle` for the same rid with attempt=0 sees `already_settled` (legacy match) but for attempt=1 sees `go` (new attempt is genuinely new work).

**【不做什么 DON'T】**
- ❌ Do NOT default `attempt` to `0` at the helper level. Force callers to pass `rec.AttemptIndex` explicitly; defaulting hides bugs.
- ❌ Do NOT remove the four-arg Lua key list; the order is load-bearing.
- ❌ Do NOT use `fmt.Sprintf("%s:%d", ...)` in the hot path; concatenation with `strconv.Itoa` allocates less.

**【怎么做 HOW (literal)】**
1. Read existing key builders:
   ```bash
   sed -n '55,62p' /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/billing/business/dedup.go
   ```
2. Replace the five key-builder funcs:
   ```go
   func dedupKey(requestID string, attempt int32) string {
       return "settled:" + requestID + ":" + strconv.Itoa(int(attempt))
   }
   func settledDoneKey(requestID string, attempt int32) string {
       return "settled:done:" + requestID + ":" + strconv.Itoa(int(attempt))
   }
   func settlingKey(requestID string, attempt int32) string {
       return "settling:" + requestID + ":" + strconv.Itoa(int(attempt))
   }
   func legacyDedupKey(requestID string, attempt int32) string {
       return dedupKey(requestID, attempt)
   }
   func pendingLegacyKey(requestID string, attempt int32) string {
       return "settled:pending:" + requestID + ":" + strconv.Itoa(int(attempt))
   }

   // legacyAttemptlessDoneKey returns the pre-WS-8 single-segment key for the
   // 26h migration window. After that window expires, delete the helper + its
   // callers in a follow-up PR.
   func legacyAttemptlessDoneKey(requestID string) string {
       return "settled:done:" + requestID
   }
   ```
3. Update `TryStartSettle` signature to `(ctx, rid string, attempt int32, settlingTTL time.Duration)`. Extend the Lua script to also probe `legacyAttemptlessDoneKey(rid)` (pass as 5th key) and return `already_settled` if present.
4. Find callers:
   ```bash
   grep -rn "TryStartSettle\|MarkSettled\|AbortSettle\|ReleaseSettling\|ClaimCostRanksSettling" /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/modules/
   ```
   Update each call site to pass `rec.AttemptIndex` (a Go `int`; cast `int32(rec.AttemptIndex)`).
5. Tests in `dedup_test.go`: add `TestDedup_KeyShape_WithAttempt` confirming the four variants format correctly; `TestDedup_LegacyAttemptlessFallback` seeding the old key and confirming both behaviors.

**【如何验证 VERIFY】**
1. ```bash
   go test -race ./modules/billing/business/ -run "TestDedup|TestHandler|TestInflightReaper" -count=5
   ```
   Expected: green; especially the inflight_reaper tests that exercise multi-attempt scenarios.
2. ```bash
   make e2e 2>&1 | grep -i "billing\|settler" | tail -50
   ```
   Expected: no `already_settled` outcomes on cross-attempt retries (chaos scenarios that kill mid-stream and retry).
3. Manual Redis inspection during e2e:
   ```bash
   docker compose exec redis redis-cli --scan --pattern "settled:*"
   ```
   Expected: every settled key has at least three colons (`settled[:done|:pending]:<rid>:<attempt>`).

**【Dependencies / Adversarial review】**
- Co-PR with T04 (same file) and WS-1 T22 (same file, same logical change). Track A1 owns the combined PR.
- Adversarial review: sonnet must construct a deploy-window scenario where Rust is still emitting records without `attempt_index` (because it hasn't deployed yet) and Go is on the new key shape. Per v2 REV-FINAL-H8.9, Rust deploys FIRST. If Rust hasn't deployed, Go reading `rec.AttemptIndex == 0` (Go zero value) would settle every retry under the attempt=0 key — duplicating settles across attempts. The legacy-attemptless probe MUST cover this case. Test it.

---

### TASK W8-T07: CI gate `RustpactMaxSupportedVersion` ≥ Rust HEAD `_pv`

**【是什么 What it IS】**
- A CI-only task. Create `shared/rustpact/version.go` (new) exposing `const MaxSupportedPV int32 = 2` and a `RustpactMinSupportedVersion int32 = 1`. Create `scripts/check-rustpact-pv.sh` that parses `crates/gw-core/src/usage.rs` for the `CURRENT_PV` constant value and asserts `Go.MaxSupportedPV >= Rust.CURRENT_PV`. Wire it into `Makefile` `check` target and `.github/workflows/contract-sync.yml` (which T01 created).
- Also create `scripts/check-deploy-order.sh` per v2 REV-FINAL-H8.9 — flags when a Rust PR raises `CURRENT_PV` without a matching Go-side bump.

**【不是什么 What it is NOT】**
- ❌ Does NOT replace the runtime `Usage.Validate()` check; CI is belt, runtime is suspenders.
- ❌ Does NOT enforce the inverse (Go raising `MaxSupportedPV` without Rust). Go leading is safe (forward-compat); Rust leading is unsafe (silent data loss).

**【为什么 Why this exists / why bug occurs】**
- Cited finding: C11 ("no protocol version → silent data loss"); v2 REV-006 §7 #12.

**【做什么 DO】**
1. Create `shared/rustpact/version.go`:
   ```go
   package rustpact

   // MaxSupportedPV is the highest UsageSnapshot._pv this Go binary can decode.
   // Bump in lockstep with crates/gw-core/src/usage.rs UsageSnapshot::CURRENT_PV.
   // CI check: scripts/check-rustpact-pv.sh.
   const MaxSupportedPV int32 = 2
   const MinSupportedPV int32 = 1
   ```
2. Create `scripts/check-rustpact-pv.sh`:
   ```bash
   #!/usr/bin/env bash
   set -euo pipefail
   cd "$(dirname "$0")/.."
   GO_MAX=$(grep -E 'MaxSupportedPV\s*int32\s*=' shared/rustpact/version.go | awk '{print $NF}')
   RUST_USAGE="../ailinkmax-gateway/crates/gw-core/src/usage.rs"
   if [[ ! -f "$RUST_USAGE" ]]; then
     echo "skip: rust sibling not present" >&2; exit 0
   fi
   RUST_CUR=$(grep -E 'CURRENT_PV\s*:\s*u32\s*=' "$RUST_USAGE" | grep -oE '[0-9]+' | tail -1)
   if (( GO_MAX < RUST_CUR )); then
     echo "FAIL: Go MaxSupportedPV ($GO_MAX) < Rust CURRENT_PV ($RUST_CUR). Bump shared/rustpact/version.go first." >&2
     exit 1
   fi
   echo "OK: Go MaxSupportedPV=$GO_MAX, Rust CURRENT_PV=$RUST_CUR"
   ```
3. Create matching `scripts/check-rustpact-pv.ps1`.
4. Wire into `Makefile` `check` target and `make.ps1`.

**【不做什么 DON'T】**
- ❌ Do NOT parse `usage.rs` with regex more permissive than `CURRENT_PV\s*:\s*u32\s*=`; otherwise a stray comment trips it.
- ❌ Do NOT make the check optional in CI; it's a launch gate.

**【怎么做 HOW (literal)】**
- See files above. Add `bash scripts/check-rustpact-pv.sh` to the `check:` target body in `Makefile`.

**【如何验证 VERIFY】**
1. ```bash
   bash scripts/check-rustpact-pv.sh
   ```
   Expected: `OK: Go MaxSupportedPV=2, Rust CURRENT_PV=2`.
2. Manually edit `usage.rs` `CURRENT_PV = 3` and re-run; expected `FAIL`.

**【Dependencies / Adversarial review】**
- Blocked by T02.

---

### TASK W8-T08: `request_id` Rust-allocated + `X-Request-Id` response header

**【是什么 What it IS】**
- Rust adds `X-Request-Id: <rid>` to every response (success and error). Docs codify that Rust is the sole allocator of `request_id`; any client-supplied `X-Request-Id` header enters trace only, NEVER billing dedup.

**【不是什么 What it is NOT】**
- ❌ Does NOT change how `request_id` is constructed.
- ❌ Does NOT pass client `X-Request-Id` to `ctx.request_id`. Per existing source (`crates/gw-proxy/src/filters/request.rs:211` uses `ctx.request_id.0` which was set at early-filter time, independent of the incoming header).

**【为什么 Why this exists / why bug occurs】**
- Cited finding: C5 (must declare Rust-allocated).

**【做什么 DO】**
1. In `crates/gw-proxy/src/filters/response.rs` (response filter), add the header to the response builder: `response_headers.insert(header::HeaderName::from_static("x-request-id"), ctx.request_id.0.parse().unwrap())`.
2. Document in `docs/rust-contract/parts/02-go-as-consumer.md` §2.1 a new subsection "request_id provenance": Rust is sole allocator; client-supplied `X-Request-Id` is observed for trace only.
3. Add a Rust integration test asserting the response carries `X-Request-Id` and it matches `ctx.request_id.0`.

**【不做什么 DON'T】**
- ❌ Do NOT echo a client-supplied `X-Request-Id` value back as the response header — always use Rust's own.

**【怎么做 HOW (literal)】**
- See above. Test file: `crates/gw-proxy/tests/x_request_id.rs`.

**【如何验证 VERIFY】**
1. ```bash
   cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
   cargo test -p gw-proxy --test x_request_id
   ```
2. Manual:
   ```bash
   curl -sv http://localhost:8000/v1/chat/completions -H 'x-request-id: client-supplied' ... 2>&1 | grep -i 'x-request-id'
   ```
   Expected: response carries an `X-Request-Id:` whose value is NOT `client-supplied`.

---

### TASK W8-T09: `model_pricing` two-phase nullable + settler NULL rejection

**【是什么 What it IS】**
- Extend Go's pricing surface with 8 new per-token columns (nullable) for the new dimensions, and make the settler refuse to compute cost when ANY new dimension is non-null in the usage record but its price column is NULL.
- Per REV-LOCAL-A (above), the real pricing surface is `global_models.default_tiered_pricing` (JSONB) + `modules/model/business/pricing.go` `BillingPricing` struct, not a non-existent `model_pricing.sql`. The 8 new fields go into `BillingPricing` as `*int64` (nullable per-1M-token price); the migration adds 8 nullable JSONB keys to `default_tiered_pricing.tier[0]`.

**【不是什么 What it is NOT】**
- ❌ Does NOT make existing 6-dim pricing nullable; those have always been required.
- ❌ Does NOT auto-backfill; ops must populate the new columns model-by-model.

**【为什么 Why this exists / why bug occurs】**
- Cited finding: v1 S8.9, v2 REV-FINAL-H8.5.

**【做什么 DO】**
1. Migration `migrations/00043_global_models_extended_pricing.sql` (new) — adds a JSONB schema check (no schema change, but a comment documenting that 8 new keys are expected: `audio_input_per_1m`, `audio_output_per_1m`, `accepted_prediction_per_1m`, `rejected_prediction_per_1m`, `cache_creation_5m_per_1m`, `cache_creation_1h_per_1m`, `server_tool_web_search_per_call`, `server_tool_web_fetch_per_call`).
2. `shared/contracts/pricing.go`: extend `ChargeInput` with 8 new `int64` token-count fields and `CostBreakdown` with 8 matching `*int64` cost fields (pointer so NULL pricing → NULL cost is distinguishable from zero cost).
3. `modules/model/business/pricing.go`: extend `BillingPricing` with 8 `*int64` `*Per1M` fields. Extend `estimateFromPricing` (line 79) to compute each new cost ONLY when both the token count is positive AND the matching `*Per1M` is non-nil. If a token count is positive but `*Per1M` is nil, return new `ErrPricingMissingDimension` (add to `shared/contracts/pricing.go`).
4. `modules/billing/business/handler.go` settler call site (line 175 `h.deps.Pricing.EstimateMicros`): on `ErrPricingMissingDimension`, do NOT settle; emit `settler.MissingPricingTotal++` Prom counter and a structured log; the record goes to DLQ via the existing DLQ path.

**【不做什么 DON'T】**
- ❌ Do NOT default missing `*Per1M` to 0; that silently undercharges. Fail-closed.
- ❌ Do NOT block the existing 6-dim path on the new fields.

**【怎么做 HOW (literal)】**
- See above. Migration is goose-style:
  ```sql
  -- +goose Up
  -- The 8 new dimensions live as optional keys inside default_tiered_pricing.tier[0].
  -- No DDL change; this migration only registers the schema expectation in a
  -- comment so audits can grep for it. Backfill is operational.
  COMMENT ON COLUMN global_models.default_tiered_pricing IS
    'jsonb {tier: [{input_per_1m, output_per_1m, cache_read_per_1m, cache_creation_per_1m, thinking_per_1m, multimodal_per_1m, audio_input_per_1m?, audio_output_per_1m?, accepted_prediction_per_1m?, rejected_prediction_per_1m?, cache_creation_5m_per_1m?, cache_creation_1h_per_1m?, server_tool_web_search_per_call?, server_tool_web_fetch_per_call?}]}';
  -- +goose Down
  COMMENT ON COLUMN global_models.default_tiered_pricing IS NULL;
  ```

**【如何验证 VERIFY】**
1. ```bash
   cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
   go test -race ./modules/model/business/ ./modules/billing/business/ -count=3
   ```
2. e2e: emit a usage record with `audio_input_tokens=100` against a model that doesn't have `audio_input_per_1m` set. Confirm settler DLQs the record and increments `MissingPricingTotal`.

**【Dependencies / Adversarial review】**
- Blocked by T02.

---

### TASK W8-T10: Decline `dup_safe` (C6)

**【是什么 What it IS】**
- A docs-only task. Reject the v1-era proposal to add a `dup_safe` boolean to `UsageRecord`. Reason: provenance ladder + dedup keys already arbitrate duplicates; `dup_safe` would duplicate the arbitration and create a state oracle.

**【做什么 DO】**
1. Add a paragraph to `docs/rust-contract/parts/02-go-as-consumer.md` §2.1 explicitly rejecting `dup_safe` and citing v2 H8.8.
2. Cross-reference from v2 plan §4 WS-8 H8.8 to this docs change.

**【如何验证 VERIFY】**
- `grep "dup_safe" docs/rust-contract/parts/02-go-as-consumer.md` returns a paragraph explaining the decline.

---

## 4. DoD (per v2 REV-005, concrete thresholds + tools)

The WS-8 PR train (T01 → T10) is mergeable only when ALL of:

1. **T01** PR merged; `contract-sync-additive` CI green for ≥ 2 consecutive PRs.
2. **T02** Rust + Go unit tests pass `-count=10 -race`; 0 occurrences of `DisallowUnknownFields` introduced.
3. **T03** Soak smoke (`make e2e-soak-smoke`) emits non-null `audio_input_tokens` for the audio scenario AND non-null `cache_creation_5m_tokens` for the Anthropic cached scenario. Verified by `jq '.[].usage.audio_input_tokens // empty' tests/e2e/client/.soak_report/*.jsonl | wc -l > 0`.
4. **T04** `dedup_test.go` `TestProvenanceRank_CliExtractedHasRank8` green; Redis state in a 5-min mixed-load run contains BOTH `v2:` and `v3:` keys without errors.
5. **T05** Soak run: `gw_codec_cross_render_lossy_total{from="anthropic",to="gemini",field="tool_call.arguments"}` is exactly 0 over 60s × 833 rps.
6. **T06** `make e2e` chaos scenario "Pingora kill mid-stream then retry" produces 0 `already_settled` for the retried attempt.
7. **T07** `bash scripts/check-rustpact-pv.sh` exits 0; deliberate breakage exits 1.
8. **T08** Every response in `tests/e2e/client/.soak_report/` has an `X-Request-Id` header equal to the request's `ctx.request_id` (parsable from harness log).
9. **T09** `ErrPricingMissingDimension` reaches DLQ when triggered; settler does not produce a cost record on missing pricing.
10. **T10** Docs paragraph present; v2 H8.8 closed.
11. **Cross-version migration test** (WS-13 chaos scenario `mixed-deploy-rust-leads-go`): with Rust at `_pv: 2` emitting and Go still at `MaxSupportedPV: 1`, Go must DLQ those records — NOT silently drop them. Verified by `gw_billing_unsupported_pv_dlq_total > 0` and `gw_billing_silent_drop_total == 0`.
12. Adversarial sonnet review (post-implementation) of each task's 7-section spec returns 0 unaddressed critiques.

---

## 5. Dependencies & sequencing inside WS-8

```
T01 (sync baseline)
   └── T02 (schema + _pv)
         ├── T03 (codec wiring)        ─── T05 (ToolCallEnd)
         ├── T04 (provenance ladder)   ┐
         ├── T06 (dedup key BNF)       ┼── must land in one PR (same file)
         │   └── co-PR with WS-1 T22   ┘
         ├── T07 (CI gate)
         └── T09 (pricing two-phase)

T08 (request_id + X-Request-Id)  independent
T10 (decline dup_safe)           independent
```

External dependencies:
- T06 ⇄ WS-1 T22 (same file). Track A1 owns the combined PR.
- T04 + T06 ⇄ same file (`dedup.go`); co-PR mandatory.
- T09 depends on `BillingPricing` not being mid-refactor by other tracks during this window.

---

## 6. Open INVESTIGATE items (referenced from v2 §99)

| ID | Question | Owner |
|---|---|---|
| INVESTIGATE-W8-A | Are there any other Go consumers of `UsageRecord.Usage` besides `modules/billing/business/handler.go`? If so, they all need T02's mirroring. Run `grep -rn rustpact.Usage modules/`. | T02 owner |
| INVESTIGATE-W8-B | Does Rust ever emit `_pv` absent (e.g. from a hot reload of a cached struct)? If so, T02's `default = "default_pv"` fires; confirm this default is `1`, not `0`. | T02 owner |
| INVESTIGATE-W8-C | Does the `v2:` parse branch in `claimCostScript` need to remain forever (some long-TTL backlog)? T04 assumes 26h is enough; verify by reading `d.ttl` callers. | T04 owner |

Each must close with a paragraph in this file (or the appropriate INVESTIGATE doc) before its parent task merges.

---

## 7. Rollback plan (WS-15 handoff)

- **T01**: revert PR; sync script reverts to old behavior; no data implications.
- **T02**: revert Rust extension first (stops emitting `_pv: 2`); then revert Go. Mid-window records with `_pv: 2` will be parsed by reverted Go as `_pv: 1` (the default), and the 8 new fields will be ignored. No data loss; new fields not billed (acceptable; the old code never billed them either).
- **T03**: revert reverts to IR→Snapshot narrow copy; new fields again zero.
- **T04**: revert with care — any `v3:` keys written before revert become unreadable to the old binary. Mitigation: 24h soak between T04 deploy and any revert; or write a one-off `v3:`→`v2:` rewriter as part of the revert PR.
- **T05**: revert removes `ToolCallEnd`; cross-render goes back to per-delta best-effort. No data loss in same-protocol path.
- **T06**: revert reverts the keys-with-attempt change; legacy attemptless probe still in place so no key is orphaned. ATTEMPT: any settle that happened under `settled:rid:1` (attempt 1) becomes invisible to the reverted binary which only looks at `settled:rid`. The retried attempt would silently re-settle — same bug WS-8 set out to fix. Practical implication: T06 is one-way; reverts require a full operational drain.
- **T07**, **T08**, **T10**: trivial revert.
- **T09**: revert leaves settler in "no new dimensions billed" state.

— End of WS-8 sub-plan —
