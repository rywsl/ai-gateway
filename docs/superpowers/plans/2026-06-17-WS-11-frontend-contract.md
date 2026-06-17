# WS-11 — Frontend Contract Coordination (TDD sub-plan)

> Parent plan: `docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §5 WS-11 + §4 WS-5/WS-6/WS-8 + v1 §4 WS-1/WS-5/WS-6.
> Format: every task obeys the v2 §0 8-section template (IS / IS NOT / WHY / DO / DON'T / HOW / VERIFY / DEPENDENCIES+ADVERSARIAL).
> Audience: a frontend TypeScript engineer with no Rust/Go background. All cited code paths are reproduced or linked; no "see code" hand-waves.
> Mirror: `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-11-frontend-contract.md` (byte-identical; cross-repo visibility per v1 §2.1).

---

## 0. Pre-flight verification (executor MUST do BEFORE coding)

Per v2 §0 重要 2: sonnet output is **hypothesis, not conclusion**. Before starting any task below, the executor MUST:

1. Read each `file:line` cited in §1 (the API change inventory). If the literal code disagrees with the description here, **source code wins**; record the divergence in `docs/issues/2026-06-17-investigates/WS-11-discrepancy.md` and adjust the affected task.
2. INVESTIGATE-012 (open): `frontend/src/api/client.ts:213` currently treats `status===403 && body.detail==='license_required'` as the license trigger. The Rust+Go gateway design (v1 §S5.1, v2 REV-FINAL-V13) specifies **HTTP 402 + `X-Reason: license_required` response header**. This sub-plan assumes the gateway lands the 402 first (T-WS11-02), then the frontend follows (T-WS11-04). Verify the Go-side license middleware PR is merged and emits 402 before merging the frontend swap.
3. INVESTIGATE-013 (open): `ErrPaymentStillProcessing` does not yet exist in `modules/wallet/business/service.go` — only refund-related code is at line 539. v1 §S1.* introduces it. Verify the Go error sentinel is added (WS-1 owner) before frontend wires `payment_still_processing` UX.
4. Verified facts (already cross-checked against repo):
   - `GW_UPSTREAM_PROTOCOL_DECODE_ERROR` constant exists: `ailinkmax-gateway/crates/gw-core/src/error/codes.rs:97`.
   - Six distinct mgmt-token codes exist today: `codes.rs:132-137` (`GW_AUTH_MGMT_TOKEN_MISSING|INVALID|EXPIRED|DISABLED|REVOKED|SCOPE_DENIED`). The collapse to single `GW_AUTH_FAILED` is a v1 §S5.18 design change; the Rust constant `GW_AUTH_FAILED` does **not yet exist** — confirm it lands as part of WS-5 F2 Track before merging T-WS11-05.
   - `e2e-fake-web/src/store.rs:55` comment already notes a similar collapse intent for JWT codes (`GW_AUTH_JWT_INVALID_ISS/AUD` → `GW_AUTH_INVALID_JWT`); cross-check the same pattern applies to mgmt collapse.

---

## 1. API Change Inventory (all wire-format changes from v1 + v2 REV-FINAL that touch frontend)

| # | Change ID | Source spec | Wire-format delta | Frontend impact | Tasks |
|---|---|---|---|---|---|
| 1 | R7 / S1.20 / T17 | v1 §S1.20 line 334-336, §T17 line 382 | Cross-protocol parse failure: was `200 OK + empty body`, becomes `502 + body.error_code = "GW_UPSTREAM_PROTOCOL_DECODE_ERROR"` | Proxy callers (none in admin UI — proxy is data-plane; verify with §1.a) must surface 502 instead of treating empty 200 as success | T-WS11-06 |
| 2 | R25 / S5.18 | v1 §S5.18 line 647, v2 REV-004c | Six mgmt-token outcomes (`MGMT_TOKEN_MISSING/INVALID/EXPIRED/DISABLED/REVOKED/SCOPE_DENIED`) collapse at HTTP layer to single `GW_AUTH_FAILED` | mgmt-token admin UI must stop branching on the six codes; show generic "authentication failed" message. Server-log preserves detail. | T-WS11-05 |
| 3 | License / S5.1 / REV-FINAL-V13 | v1 §S5.1 line 611, v2 REV-FINAL-V13, REV-004e | Non-whitelist + unlicensed: was 403 (current frontend assumption) → **402 + response header `X-Reason: license_required`**. Whitelist: `/livez`, `/readyz`, `/api/license/status`, `/api/license/machine`, `/api/license/activate`, `/api/auth/login`, `/api/public/*` | `apiClient.handleResponseError` must branch on `status===402 && headers['x-reason']==='license_required'` (replace current 403+body.detail check). License-warn mode (REV-004e) emits 200 but adds `X-License-Warning: degraded_<reason>` — frontend shows non-blocking banner. | T-WS11-04 |
| 4 | R2 / S5.14 / REV-FINAL-V21 | v1 §S5.14 line 639, v2 REV-FINAL-V21 | HMAC-signed requests (used only by management-token-signed callers — NOT the bearer-token web UI): add request header `X-Nonce: <uuid v4>`. Replay → `403 + body.error_code = "GW_AUTH_REPLAY_DETECTED"`. Per V21, `replay_check` is operator-configurable enum `{none, nonce_redis, nonce_local}`, default `none`. | HMAC clients (Rust gateway management probes, signed scripts) need nonce header; web admin uses Bearer JWT and is unaffected — but `tests/contract` and any `management-tokens.ts`-signed callsite must be audited. | T-WS11-07 |
| 5 | REV-FINAL-H8.8 / X-Request-Id | v2 REV-FINAL-H8.8 | Every gateway response gains response header `X-Request-Id: <rust-allocated-uuid>`. Client-supplied `X-Request-Id` on the request is allowed for trace correlation but **must not** be re-emitted as if Rust-allocated, and must **not** be used as billing dedup key. | Frontend SHOULD log `X-Request-Id` from every response for support-ticket correlation. Admin "request trace" feature (`requestTrace.ts`) should expose it. Tests must assert presence. | T-WS11-08 |
| 6 | ErrPaymentStillProcessing | v1 §4 WS-1 line 268 | Refund attempted while corresponding `payment_outbox` row is `pending`: `409 Conflict + body.error_code = "PAYMENT_STILL_PROCESSING"` (suggested HTTP mapping; verify Go-side picks 409 not 422 — INVESTIGATE-013). | Admin payments / wallet UI: surface "Payment still settling — retry in a moment" with retry-after backoff. Currently no error path for this; calls to `/api/admin/payments/.../refund` need handling. | T-WS11-09 |
| 7 | C4 / dedup attempt_index | v1 §C4, v2 REV-FINAL-H8.9 | Proxy data plane only; no admin-UI surface. Listed for completeness so executor doesn't chase a phantom frontend task. | None | (out of scope, log only) |
| 8 | R8 reset_response_attempt_state | v1 §R8 line 95, v2 REV-FINAL-H6.3 | Proxy data plane; client sees consistent per-attempt body. No control-plane surface. | None — listed for completeness. | (out of scope, log only) |

### 1.a Inventory delta against current frontend (verified)

- `frontend/src/api/client.ts:213` checks `status === 403 && rawDetail === 'license_required'` — confirm-and-fix target of T-WS11-04.
- `frontend/src/api/license.ts` only covers control-plane endpoints (`status`/`machine`/`activate`); no enforcement-trigger handling there — enforcement lives in `client.ts` interceptor.
- No frontend file currently references `GW_UPSTREAM_PROTOCOL_DECODE_ERROR`, `GW_AUTH_FAILED`, `GW_AUTH_REPLAY_DETECTED`, `X-Nonce`, or `X-Request-Id`. Greenfield additions.
- `frontend/src/api/management-tokens.ts` exists — audit it in T-WS11-07 for HMAC-signed paths.
- `tests/contract/frontend.go` (224 lines) scans `frontend/src/api/*.ts` for endpoint patterns; T-WS11-03 ensures any new endpoints (none expected in WS-11, but `/api/license/activate` etc. must remain registered) stay in sync.
- `frontend/src/api/__tests__/client.spec.ts` is the vitest harness — T-WS11-04 and T-WS11-08 add cases there.

---

## 2. Task list (TDD, 8-section format)

### T-WS11-01 — Establish frontend contract baseline + lint-grade type module

**【是什么 What it IS】**
- A new file `frontend/src/api/contract/errors.ts` that exports:
  - `GatewayErrorCode` discriminated union (literal strings for every code reachable to frontend),
  - `LICENSE_WHITELIST_PATHS` (the 7 paths from REV-FINAL-V13),
  - helper `isGatewayErrorBody(x: unknown): x is GatewayErrorBody`,
  - `LICENSE_REASON_HEADER = 'X-Reason'`, `REQUEST_ID_HEADER = 'X-Request-Id'`, `NONCE_HEADER = 'X-Nonce'`, `LICENSE_WARNING_HEADER = 'X-License-Warning'` constants.
- A new file `frontend/src/api/contract/index.ts` re-exporting the above.

**【不是什么 What it is NOT】**
- Not a renderer (no React, no toasts).
- Not a runtime parser for arbitrary bodies — only narrowing.
- Does NOT add or remove endpoints in any `*.ts` API file.

**【为什么 Why this exists】**
- v1 §S5.18 collapses mgmt codes; v2 REV-FINAL-H8.8 adds X-Request-Id; v1 §S1.20 adds GW_UPSTREAM_PROTOCOL_DECODE_ERROR. A single source of truth prevents 30+ string-literal "GW_AUTH_FAILED" sprinkled across components going out of sync.
- Without this module, T-WS11-04..09 would each invent their own header-key strings and code constants — exactly the drift WS-11 is meant to prevent.

**【做什么 DO】**
1. Create `frontend/src/api/contract/errors.ts`. Export every code referenced in §1 of this plan: `GW_UPSTREAM_PROTOCOL_DECODE_ERROR`, `GW_AUTH_FAILED`, `GW_AUTH_REPLAY_DETECTED`, `PAYMENT_STILL_PROCESSING`.
2. Export `LICENSE_REASON_HEADER`, `REQUEST_ID_HEADER`, `NONCE_HEADER`, `LICENSE_WARNING_HEADER`.
3. Export `LICENSE_WHITELIST_PATHS = ['/livez', '/readyz', '/api/license/status', '/api/license/machine', '/api/license/activate', '/api/auth/login'] as const` plus a `LICENSE_WHITELIST_PREFIXES = ['/api/public/'] as const`.
4. Add `isLicenseWhitelisted(url: string): boolean` matching exact paths in WHITELIST_PATHS or any startsWith in PREFIXES.
5. Add `isGatewayErrorBody` user-defined type guard checking `typeof body === 'object' && body !== null && typeof (body as {error_code?: unknown}).error_code === 'string'`.

**【不做什么 DON'T】**
- ❌ Do NOT export a runtime enum (`enum GatewayErrorCode`) — tree-shake-unfriendly; use string literal union + `as const` arrays.
- ❌ Do NOT couple to axios in this module — keep it framework-free so it can be reused by mocks.
- ❌ Do NOT add new error codes "just in case". Every constant must trace to a row in §1.
- ❌ Do NOT lowercase header constants — axios normalizes incoming headers to lower-case but we compare via `headers[KEY.toLowerCase()]`; document this in a comment on the constant.

**【怎么做 HOW (具体)】**

```ts
// frontend/src/api/contract/errors.ts
// Header constants: axios normalizes response header keys to lowercase, so
// always read via `response.headers[FOO_HEADER.toLowerCase()]`.
export const LICENSE_REASON_HEADER = 'X-Reason' as const
export const REQUEST_ID_HEADER = 'X-Request-Id' as const
export const NONCE_HEADER = 'X-Nonce' as const
export const LICENSE_WARNING_HEADER = 'X-License-Warning' as const

export const GATEWAY_ERROR_CODES = [
  'GW_UPSTREAM_PROTOCOL_DECODE_ERROR', // v1 §S1.20 / T17, was 200 → 502
  'GW_AUTH_FAILED',                    // v1 §S5.18, mgmt code collapse
  'GW_AUTH_REPLAY_DETECTED',           // v1 §S5.14, HMAC nonce reuse
  'PAYMENT_STILL_PROCESSING',          // v1 §4 WS-1 line 268, outbox pending
] as const
export type GatewayErrorCode = (typeof GATEWAY_ERROR_CODES)[number]

export interface GatewayErrorBody {
  error_code: GatewayErrorCode | string // server may return unknown future codes
  detail?: string
  request_id?: string
}

export function isGatewayErrorBody(x: unknown): x is GatewayErrorBody {
  return typeof x === 'object' && x !== null
    && typeof (x as { error_code?: unknown }).error_code === 'string'
}

export const LICENSE_WHITELIST_PATHS = [
  '/livez', '/readyz',
  '/api/license/status', '/api/license/machine', '/api/license/activate',
  '/api/auth/login',
] as const
export const LICENSE_WHITELIST_PREFIXES = ['/api/public/'] as const

export function isLicenseWhitelisted(url: string): boolean {
  if ((LICENSE_WHITELIST_PATHS as readonly string[]).includes(url)) return true
  return LICENSE_WHITELIST_PREFIXES.some((p) => url.startsWith(p))
}
```

**【如何验证 VERIFY】**
1. `cd frontend && pnpm tsc --noEmit`: zero TypeScript errors.
2. `pnpm vitest run src/api/contract/__tests__/errors.spec.ts -t "whitelist"`: new spec adds 7 inclusion cases + 3 prefix cases + 2 negative cases; expect `10 passed`.
3. `pnpm eslint src/api/contract/`: zero warnings.
4. Output file `logs/T-WS11-01-verify.txt` containing `PASS` and `0 errors`.

**【依赖 + 对抗复审】**
- Depends on: none (greenfield).
- Blocks: T-WS11-04, T-WS11-05, T-WS11-06, T-WS11-08, T-WS11-09.
- Adversarial review prompt: ask sonnet to (a) audit whether any whitelist path is missing (compare against v1 line 611 + v2 REV-FINAL-V13), (b) confirm `isGatewayErrorBody` correctly handles `null` and arrays, (c) verify `as const` widening rules don't break consumer assignability.

---

### T-WS11-02 — VERIFY-ONLY: Confirm Go license middleware emits 402 + X-Reason (blocking gate)

**【是什么 What it IS】**
- A read-only verification step performed against the Go repo. NOT a code change in this PR.
- Produces a one-page `docs/issues/2026-06-17-investigates/WS-11-license-402-confirmed.md` with literal request/response transcripts.

**【不是什么 What it is NOT】**
- Not an implementation task (WS-5 Track F1 owns the Go middleware).
- Not optional — without 402 landing first, T-WS11-04 has nothing to integrate against.

**【为什么 Why this exists】**
- v1 §S5.1 specifies 402 + `X-Reason: license_required`.
- Current frontend `client.ts:213` reads 403 + body. If the Go side ships 402 but frontend still expects 403, every authenticated request post-cutover hangs.
- v2 §0 重要 2: assume nothing without source-read.

**【做什么 DO】**
1. After WS-5 Track F1 merges its license-middleware PR, run the gateway locally (`make run-web`) with a no-license-file install.
2. Issue `curl -i -H 'Authorization: Bearer <valid-jwt>' http://localhost:8080/api/users/me`.
3. Record literal output. Assert: status line is `HTTP/1.1 402 Payment Required`, header `X-Reason: license_required`, body contains `"error_code": "license_required"` (or whatever Go middleware emits — record verbatim).
4. Repeat for each whitelist path; assert all return 200 even unlicensed.
5. File the transcript in `docs/issues/2026-06-17-investigates/WS-11-license-402-confirmed.md`.

**【不做什么 DON'T】**
- ❌ Do NOT start T-WS11-04 implementation before this verification lands.
- ❌ Do NOT mock the response — must hit a real Go binary.
- ❌ Do NOT assume the body shape — record what Go actually returns; we will adapt T-WS11-04 to it.

**【怎么做 HOW (具体)】**

```bash
# from AILinkMax-Go root
make run-web &
sleep 3
TOKEN=$(curl -s -X POST localhost:8080/api/auth/login -d '{"email":"smoke@test","password":"x"}' | jq -r .access_token)
curl -is -H "Authorization: Bearer $TOKEN" localhost:8080/api/users/me \
  | tee logs/T-WS11-02-license-trigger.txt
# Expect status 402, header `X-Reason: license_required`
grep -E '^HTTP/1\.1 402 ' logs/T-WS11-02-license-trigger.txt
grep -iE '^X-Reason: license_required' logs/T-WS11-02-license-trigger.txt
# Whitelist sample
curl -is localhost:8080/api/license/status | head -1 | grep '200'
curl -is localhost:8080/livez | head -1 | grep '200'
```

**【如何验证 VERIFY】**
1. All three `grep` exit 0.
2. File `docs/issues/2026-06-17-investigates/WS-11-license-402-confirmed.md` exists and contains the captured transcripts.
3. If status is **not** 402 (e.g. Go shipped 403 — alignment drift), open `INVESTIGATE-012-followup.md` and escalate to WS-5 F1 owner; T-WS11-04 must be re-scoped to whatever Go actually emits.

**【依赖 + 对抗复审】**
- Depends on: WS-5 Track F1 license-middleware PR merged.
- Blocks: T-WS11-04.
- Adversarial review: ask sonnet to inspect the Go license middleware source (under `modules/license/` + `binaries/web/app/modules.go`) and confirm there is no second middleware downstream that overrides 402 → 403.

---

### T-WS11-03 — Lock down route contract test against accidental endpoint deletion

**【是什么 What it IS】**
- Add a test case in `tests/contract/contract_test.go` that asserts the seven license-whitelist paths plus `/api/auth/refresh` and `/api/auth/logout` remain registered, so a future refactor cannot silently drop them and turn a whitelisted GET into a permanent 402.

**【不是什么 What it is NOT】**
- Not a frontend test (it lives in Go-side tests/contract).
- Not adding new endpoints; it asserts existing endpoints are still mounted.
- Not the `make test-frontend` integration harness.

**【为什么 Why this exists】**
- `tests/contract/contract_test.go` (verified 131 lines) already scans frontend `*.ts` and asserts router has matching `chi` patterns. But there is no test guarding the **license-whitelist set**: if someone re-orders middleware and accidentally puts license **before** `/livez` mount, the gateway will 402 its own readyz probe.
- v2 REV-006 §7 #10: all PRs touching api layer must pass contract test.

**【做什么 DO】**
1. Add `TestLicenseWhitelistRegistered` to `tests/contract/contract_test.go`.
2. Reuse `RouterPatterns(r)` helper (line 39-44 of existing test) to dump real `binaries/web` router and assert each of the 7 paths is present.
3. Have the test also assert that no middleware in the chain rejects whitelist paths during an "unlicensed" simulated state — use the existing `binaries/web/app` test harness if present, otherwise just check route presence.

**【不做什么 DON'T】**
- ❌ Do NOT hardcode the seven paths inline — import the canonical list from a new Go constant in `modules/license/api/whitelist.go` (T-WS11-04 mirror — created here as a single edit) and mirror it to the TS `LICENSE_WHITELIST_PATHS` of T-WS11-01.
- ❌ Do NOT skip `/livez`/`/readyz` because they live outside `/api/*` — they MUST be in the whitelist per REV-FINAL-V13.

**【怎么做 HOW (具体)】**

```go
// modules/license/api/whitelist.go (new file, ~12 lines)
package api

// LicenseWhitelistPaths mirrors frontend/src/api/contract/errors.ts
// LICENSE_WHITELIST_PATHS. Any drift between the two lists is a defect:
// see v1 §S5.1 / v2 REV-FINAL-V13.
var LicenseWhitelistPaths = []string{
    "/livez",
    "/readyz",
    "/api/license/status",
    "/api/license/machine",
    "/api/license/activate",
    "/api/auth/login",
}

// LicenseWhitelistPrefixes mirrors LICENSE_WHITELIST_PREFIXES.
var LicenseWhitelistPrefixes = []string{
    "/api/public/",
}
```

```go
// tests/contract/contract_test.go (append)
func TestLicenseWhitelistRegistered(t *testing.T) {
    r := buildWebRouter(t) // existing helper in router.go; if absent, add a thin one
    pats, err := RouterPatterns(r)
    require.NoError(t, err)
    seen := map[string]bool{}
    for _, p := range pats { seen[p.Path] = true }
    for _, want := range api.LicenseWhitelistPaths {
        // /livez and /readyz may be mounted under root; chi normalizes them
        require.Truef(t, seen[want], "whitelist path %q not registered (REV-FINAL-V13)", want)
    }
}
```

**【如何验证 VERIFY】**
1. `cd AILinkMax-Go && go test ./tests/contract/ -run TestLicenseWhitelistRegistered -count=1`.
2. Deliberately delete a path from `LicenseWhitelistPaths` (in a scratch branch) and re-run — expect FAIL with the missing-path name.
3. `make test-frontend` continues to pass (no new endpoints introduced; only assertions added).
4. Output `logs/T-WS11-03-verify.txt` containing `ok tests/contract`.

**【依赖 + 对抗复审】**
- Depends on: T-WS11-02 confirmed.
- Blocks: T-WS11-04 (frontend reads the mirrored list via T-WS11-01).
- Adversarial review: ask sonnet to verify the Go `LicenseWhitelistPaths` slice and the TS `LICENSE_WHITELIST_PATHS` literal cannot drift silently — propose a check-script in WS-12 (`scripts/check-license-whitelist-mirror.sh`) that diffs the two via `grep | sort | diff`.

---

### T-WS11-04 — Frontend interceptor: replace 403/license_required check with 402 + X-Reason header

**【是什么 What it IS】**
- Edit `frontend/src/api/client.ts` `handleResponseError` to branch on `status === 402 && headers['x-reason'] === 'license_required'` instead of (or in addition to, for one release of overlap — see DO step 4) the current 403+body.detail path at line 207-223.
- Edit so that the warn-mode (`X-License-Warning` response header on any 2xx) drops a non-blocking event for UI banner.

**【不是什么 What it is NOT】**
- Not adding new endpoints.
- Not changing the demo-mode adapter — only the production interceptor.
- Not touching the auth-refresh flow.
- Not handling 402 on whitelist paths — those should never 402; if one does, it's a server defect (T-WS11-03 catches it).

**【为什么 Why this exists】**
- Verified: `client.ts:213` currently uses `status === 403` + `rawDetail === 'license_required'`. v1 §S5.1 specifies 402 + `X-Reason`. Without this swap, license-enforcement post-cutover blanket-breaks the admin UI.
- REV-004e: a 14-day `warn` mode precedes hard `enforce`. UI should show a banner during warn so customers act before enforce.

**【做什么 DO】**
1. Import `LICENSE_REASON_HEADER`, `LICENSE_WARNING_HEADER`, `isLicenseWhitelisted` from `@/api/contract/errors`.
2. In `handleResponseError`: replace the `status === 403 ... license_required` branch with a 402 branch that reads the response header (lower-cased) `x-reason`.
3. Add a successful-response branch (in the response interceptor's success path, around line 158-162) that detects `headers['x-license-warning']` and dispatches a CustomEvent `aether-license-warning` so the shell can show a non-blocking banner.
4. Overlap window: for 1 release, keep the legacy 403/body.detail handler **only if `import.meta.env.VITE_LICENSE_COMPAT === 'true'`** (default false). Remove the legacy branch in the release after the Go cutover is confirmed.
5. Update `frontend/src/api/__tests__/client.spec.ts`: add three tests — (a) 402 + X-Reason triggers `setLicenseDemoMode(true, 'license_required')`; (b) 402 without X-Reason does NOT trigger (e.g. real "out of credits" 402 — different code path); (c) 200 + X-License-Warning dispatches event.

**【不做什么 DON'T】**
- ❌ Do NOT trigger `setLicenseDemoMode` on every 402 — there are legitimate 402s (quota exhausted) that are not license issues; gate strictly on `X-Reason: license_required`.
- ❌ Do NOT redirect the user on 402; license_required → enter demo mode (existing behaviour), do not clearAuth.
- ❌ Do NOT read `error.response.data.detail` for the new branch — v1 §S5.1 spec is header-based; body shape is an implementation detail of the Go middleware that may change.
- ❌ Do NOT add a second axios interceptor — extend the existing one to keep refresh-token state machine intact.

**【怎么做 HOW (具体)】**

```ts
// frontend/src/api/client.ts (modify around line 207-223)
import {
  LICENSE_REASON_HEADER,
  LICENSE_WARNING_HEADER,
} from '@/api/contract/errors'

// ... inside handleResponseError, replace the 403 license_required branch:
const reasonHeader = String(
  error.response?.headers?.[LICENSE_REASON_HEADER.toLowerCase()] ?? ''
).toLowerCase()

if (status === 402 && reasonHeader === 'license_required') {
  setLicenseDemoMode(true, 'license_required')
  return Promise.reject(error)
}

// One-release backward-compat shim (removed in next release):
if (
  import.meta.env.VITE_LICENSE_COMPAT === 'true' &&
  status === 403
) {
  const rawDetail = (error.response?.data as Record<string, unknown>)?.detail
  if (typeof rawDetail === 'string' && rawDetail === 'license_required') {
    setLicenseDemoMode(true, 'license_required')
    return Promise.reject(error)
  }
}
```

```ts
// modify the success interceptor at line 158-162:
this.client.interceptors.response.use(
  (response) => {
    const warn = response.headers?.[LICENSE_WARNING_HEADER.toLowerCase()]
    if (typeof warn === 'string' && warn.length > 0 && typeof window !== 'undefined') {
      window.dispatchEvent(new CustomEvent<{ reason: string }>(
        'aether-license-warning', { detail: { reason: warn } }
      ))
    }
    return response
  },
  async (error) => this.handleResponseError(error)
)
```

**【如何验证 VERIFY】**
1. `pnpm vitest run src/api/__tests__/client.spec.ts -t "license"` — 3 new tests pass + all existing pass.
2. `pnpm vitest run` — full frontend suite green.
3. Manual integration: start Go gateway in unlicensed state (T-WS11-02 transcript), point dev frontend at it, log in → admin page → should land in demo-mode banner without redirect.
4. `make test-frontend` end-to-end: expect green (license whitelist still returns 200 → login + status accessible).
5. Output `logs/T-WS11-04-verify.txt` containing `Tests passed` for the three new cases by name.

**【依赖 + 对抗复审】**
- Depends on: T-WS11-01 (constants), T-WS11-02 (Go-side 402 confirmed), T-WS11-03 (route contract).
- Blocks: removal of the compat shim in the next release.
- Adversarial review: ask sonnet to check (a) whether any business-logic 402 elsewhere (e.g. quota) accidentally sends `X-Reason: license_required` — if yes, fix server side, this branch must be strict; (b) whether the warn-mode event listener has a leak (we only dispatch; never unregister — fine for shell); (c) whether axios lower-cases all headers or only some — `axios` versions <1.x normalize differently; verify against `node_modules/axios/package.json` version.

---

### T-WS11-05 — Frontend: handle GW_AUTH_FAILED (collapsed mgmt-token code)

**【是什么 What it IS】**
- Audit and update `frontend/src/api/management-tokens.ts`, `auth.ts`, `admin.ts` (anywhere a mgmt-token call is made) to surface a generic "Authentication failed" message when `error.response.data.error_code === 'GW_AUTH_FAILED'`, instead of branching on the six legacy codes.
- Remove any frontend code that branches on `GW_AUTH_MGMT_TOKEN_{MISSING,INVALID,EXPIRED,DISABLED,REVOKED,SCOPE_DENIED}`.

**【不是什么 What it is NOT】**
- Not a change to bearer-JWT auth (web admin sessions) — those still go through 401 + refresh.
- Not a server-side change. Server still logs the granular outcome; only HTTP exposes `GW_AUTH_FAILED`. (v1 §S5.18, v2 REV-004c.)

**【为什么 Why this exists】**
- v1 §S5.18 + v2 REV-004c: prevent state-oracle. v1 line 647 verbatim: "四种 outcomes 在 HTTP 层统一为 `GW_AUTH_FAILED`，详细原因仅写 server-side log." Verified six (not four) codes exist today in Rust — `codes.rs:132-137`.
- If frontend keeps an if-ladder on the six legacy codes, every branch becomes dead code after the cutover but worse — UX may say "token expired" for a missing token, confusing users.

**【做什么 DO】**
1. `git grep -E "GW_AUTH_MGMT_TOKEN_(MISSING|INVALID|EXPIRED|DISABLED|REVOKED|SCOPE_DENIED)" frontend/src/` → list all sites.
2. For each site, collapse the branch to a single `if (code === 'GW_AUTH_FAILED')` returning the user-facing message "Authentication failed — re-issue your management token".
3. Use the constant `GW_AUTH_FAILED` from `@/api/contract/errors` (T-WS11-01); no string literals.
4. Add vitest cases under `__tests__/management-tokens.spec.ts` if file exists; otherwise create one with 2 cases: success + auth-failed.

**【不做什么 DON'T】**
- ❌ Do NOT keep the six-way switch for "telemetry" — the whole point of REV-004c is to deny the oracle. Server log keeps the granular detail.
- ❌ Do NOT translate any of the six codes to a different user-facing message — single message for all.
- ❌ Do NOT touch `auth.ts` JWT 401 path — different concern.

**【怎么做 HOW (具体)】**

```ts
// example transformation: frontend/src/api/management-tokens.ts (hypothetical)
import { isGatewayErrorBody } from '@/api/contract/errors'

function explain(error: unknown): string {
  if (!axios.isAxiosError(error)) return 'Network error'
  const body = error.response?.data
  if (isGatewayErrorBody(body) && body.error_code === 'GW_AUTH_FAILED') {
    return 'Authentication failed — re-issue your management token.'
  }
  return 'Request failed'
}
```

**【如何验证 VERIFY】**
1. `git grep -E 'GW_AUTH_MGMT_TOKEN_' frontend/src/` returns ZERO matches after the edit.
2. `pnpm tsc --noEmit && pnpm vitest run src/api/__tests__/management-tokens.spec.ts` green.
3. INVESTIGATE-002 (v2 §99): cross-reference with WS-5 F2 owner that the Rust constant `GW_AUTH_FAILED` is published in `crates/gw-core/src/error/codes.rs` and used by the mgmt path; verified-absent in `codes.rs:132-137` today, so this task **blocks** on Rust-side landing.
4. Output `logs/T-WS11-05-verify.txt` with zero-match grep output.

**【依赖 + 对抗复审】**
- Depends on: T-WS11-01, WS-5 Track F2 publishes `GW_AUTH_FAILED` constant.
- Blocks: nothing (other than user-visible cleanup).
- Adversarial review: ask sonnet to grep for any code outside frontend (docs, e2e) that still references the six legacy codes — document the cleanup in WS-12 lint.

---

### T-WS11-06 — Handle R7 502 GW_UPSTREAM_PROTOCOL_DECODE_ERROR (proxy-callers audit)

**【是什么 What it IS】**
- Verify whether any control-plane frontend code calls the data-plane proxy directly (it shouldn't — proxy is the OpenAI/Anthropic-compatible API on a different port). If yes, add 502 handling. If no, document the negative result so a future engineer doesn't re-investigate.

**【不是什么 What it is NOT】**
- Not a change to the proxy itself.
- Not a billing path change.
- Not relevant to `/api/*` admin endpoints (which are Go web, never Rust proxy).

**【为什么 Why this exists】**
- v1 §S1.20 line 336 + §T17 line 382: parse failure was `200 OK + Ok(0)` → now `502 + GW_UPSTREAM_PROTOCOL_DECODE_ERROR`.
- v2 §0 重要 2 mandates verifying assumptions. v1 §5 row WS-6 lists R7 as Rust+Go; we need to confirm zero frontend ts code calls Rust proxy. Otherwise UI freezes on 502 it never expected.

**【做什么 DO】**
1. `git grep -E "(/v1/|/anthropic/|/openai/|messages\.json|chat/completions)" frontend/src/` — should be zero in production code.
2. If non-zero hits: for each, wrap the call site to surface `error_code === 'GW_UPSTREAM_PROTOCOL_DECODE_ERROR'` as "Upstream provider returned an unparseable response — retry or contact support."
3. If zero hits: write `docs/issues/2026-06-17-investigates/WS-11-r7-frontend-audit.md` recording the negative result with the grep transcript.

**【不做什么 DON'T】**
- ❌ Do NOT retry on 502 automatically — could double-bill (v1 R7 root cause).
- ❌ Do NOT silently swallow the 502.

**【怎么做 HOW (具体)】**

```bash
cd frontend
git grep -nE "(/v1/messages|/v1/chat/completions|/anthropic/|/openai/)" src/ | tee ../logs/T-WS11-06-grep.txt
test ! -s ../logs/T-WS11-06-grep.txt || echo "found hits; do step 2"
```

**【如何验证 VERIFY】**
1. Grep transcript saved to `logs/T-WS11-06-grep.txt`.
2. If zero hits: `docs/issues/.../WS-11-r7-frontend-audit.md` exists.
3. If non-zero hits: vitest case added showing 502+code surfaces the message; passes.

**【依赖 + 对抗复审】**
- Depends on: T-WS11-01.
- Blocks: none.
- Adversarial review: sonnet to verify the proxy is exposed on a separate port/host (typically not under frontend baseURL) and confirm `getApiBaseUrl()` never points at proxy. Read `frontend/src/utils/url.ts`.

---

### T-WS11-07 — HMAC nonce header for signed callers (audit + add X-Nonce)

**【是什么 What it IS】**
- Audit all signed-request callsites in frontend (likely zero in browser UI; possibly present in node-side dev scripts under `frontend/scripts/` or in the e2e harness).
- If any HMAC-signed client exists: add `X-Nonce: <uuid v4>` header, and on `403 + body.error_code === 'GW_AUTH_REPLAY_DETECTED'` regenerate and retry **once** (idempotent op only).

**【不是什么 What it is NOT】**
- Not a change to Bearer JWT (web UI uses JWT, no nonce).
- Not enabling `replay_check` by default — v2 REV-FINAL-V21 makes it operator-opt-in default `none`. Frontend must still emit the nonce so signing works post-opt-in.
- Not signing requests in the browser (signing secret never ships to browser).

**【为什么 Why this exists】**
- v1 §S5.14 line 639; v2 REV-FINAL-V21 — nonce header is the only client-visible new requirement when the server toggles `replay_check`. Better to emit always than scramble at toggle time.
- INVESTIGATE: HMAC nonce header name. v1 says "带 nonce (uuid v4)" without naming the header; gateway code as of `request.rs:130` (mgmt path) doesn't show a nonce parser yet. Confirm header name before merge — proposed `X-Nonce`.

**【做什么 DO】**
1. `git grep -E "(HMAC|x-signature|X-Signature|hmac)" frontend/ tests/` — list any signed client.
2. For each, add `X-Nonce: crypto.randomUUID()` to every signed request.
3. Add a single retry on 403 + `GW_AUTH_REPLAY_DETECTED` (only if the request is idempotent — GET / DELETE; never POST).
4. Pin the header name to `NONCE_HEADER` from T-WS11-01.

**【不做什么 DON'T】**
- ❌ Do NOT generate the nonce server-side — the whole point is replay protection.
- ❌ Do NOT cache the nonce; one per request.
- ❌ Do NOT retry POSTs — even on replay-detected, server may have processed it.

**【怎么做 HOW (具体)】**

```ts
// example: if signed admin script exists at frontend/scripts/admin-call.ts
import { NONCE_HEADER } from '@/api/contract/errors'
config.headers[NONCE_HEADER] = crypto.randomUUID()
```

**【如何验证 VERIFY】**
1. Negative-case audit: `git grep -E '(X-Signature|HMAC)' frontend/src/` finds zero (expected) → record in `logs/T-WS11-07-audit.txt`.
2. Positive-case (if any HMAC client found): vitest mock returns 403 + `GW_AUTH_REPLAY_DETECTED` once → retry happens once, second nonce differs.
3. INVESTIGATE-014: confirm the Rust gateway parses header `X-Nonce` (not `X-Signature-Nonce` or `X-Idem-Nonce`); read Rust HMAC code under `crates/gw-proxy/src/auth/` (search). If header name differs, update `NONCE_HEADER` constant.

**【依赖 + 对抗复审】**
- Depends on: T-WS11-01.
- Blocks: WS-5 S5.14 cutover (`replay_check=nonce_redis`).
- Adversarial review: sonnet reads `crates/gw-proxy/src/filters/request.rs` and `crates/gw-proxy/src/auth/` for the literal header name; INVESTIGATE-014 is the deliverable artifact.

---

### T-WS11-08 — Capture X-Request-Id on every response; expose in request-trace UI

**【是什么 What it IS】**
- Modify the axios success/error interceptor in `client.ts` to read `response.headers['x-request-id']`, store it in a per-request context attached to AxiosError, and expose it to the existing `requestTrace.ts` admin surface.

**【不是什么 What it is NOT】**
- Not changing billing/dedup logic (v2 REV-FINAL-H8.8 says client-supplied X-Request-Id is NOT billing dedup).
- Not sending X-Request-Id from the client — frontend is a consumer.
- Not a persistence change.

**【为什么 Why this exists】**
- v2 REV-FINAL-H8.8: response header `X-Request-Id` is Rust-allocated; frontend logging it dramatically eases support-ticket triage. Without it, customers reporting "this UI button failed" force engineers to grep PG/CH by timestamp.

**【做什么 DO】**
1. In `client.ts` success interceptor (modified in T-WS11-04), additionally read `response.headers['x-request-id']` and attach to a WeakMap keyed by the response or on `response.config` metadata.
2. In the error path, attach the same to `error.config.metadata.requestId` if present.
3. Update `requestTrace.ts` to expose the latest 50 (requestId, url, status, ts) tuples for the in-app trace viewer.
4. Update `frontend/src/api/__tests__/client.spec.ts` with one test that mocks a response with `x-request-id: abc-123` and asserts the value is observable downstream.

**【不做什么 DON'T】**
- ❌ Do NOT log X-Request-Id to remote telemetry without user consent — keep client-local.
- ❌ Do NOT generate one client-side when absent — log "(none)" instead, so we notice gateway drift.
- ❌ Do NOT use the value as a React key — it's not stable across retries.

**【怎么做 HOW (具体)】**

```ts
// frontend/src/api/client.ts (extend success interceptor)
import { REQUEST_ID_HEADER } from '@/api/contract/errors'
import { recordRequestTrace } from '@/api/requestTrace'

this.client.interceptors.response.use((response) => {
  const requestId = String(
    response.headers?.[REQUEST_ID_HEADER.toLowerCase()] ?? ''
  )
  if (requestId) {
    recordRequestTrace({
      requestId,
      url: response.config.url ?? '',
      method: (response.config.method ?? 'GET').toUpperCase(),
      status: response.status,
      ts: Date.now(),
    })
  }
  return response
}, ...)
```

**【如何验证 VERIFY】**
1. `pnpm vitest run src/api/__tests__/client.spec.ts -t "request id"` green.
2. Manual: open admin UI → DevTools Network panel — confirm every response has `x-request-id`; open the in-app trace viewer (`/admin/_trace` or wherever `requestTrace` mounts) → see the same IDs.
3. `make test-frontend` green.
4. Output `logs/T-WS11-08-verify.txt`.

**【依赖 + 对抗复审】**
- Depends on: T-WS11-01.
- Blocks: none.
- Adversarial review: sonnet to verify `crates/gw-proxy/src/filters/response.rs:164/230/300/410` actually sets the header on the outgoing response (not just logs it). v2 REV-FINAL-H8.8 mandates "response header" — confirm the literal `set_header` call exists.

---

### T-WS11-09 — Handle ErrPaymentStillProcessing in refund UI

**【是什么 What it IS】**
- Update `frontend/src/api/admin-payments.ts` (and any wallet-side refund call) to handle `409 + body.error_code === 'PAYMENT_STILL_PROCESSING'` by surfacing a non-error "Payment is still settling — try again in a few seconds" toast and offering a single Retry button (with 5s exponential backoff cap).

**【不是什么 What it is NOT】**
- Not auto-retry — admin action, requires explicit click.
- Not a polling implementation — that's a future enhancement (FILL-* candidate).
- Not changing the wallet-balance display.

**【为什么 Why this exists】**
- v1 §4 WS-1 line 268: `Refund.Create` returns `ErrPaymentStillProcessing` when `payment_outbox` row for the payment is not yet `committed`. If frontend treats this as a generic 4xx, the operator sees "refund failed" and may try a workaround that double-refunds.
- INVESTIGATE-013: HTTP mapping (409 vs 422) is undefined in v1; this plan assumes 409 Conflict (the canonical "concurrent state" code). Confirm with WS-1 Track A1 owner.

**【做什么 DO】**
1. Add the constant `PAYMENT_STILL_PROCESSING` to T-WS11-01 (already listed).
2. In `admin-payments.ts`, wrap refund POST in a function returning a discriminated union `{ok: true, ...} | {ok: false, kind: 'still_processing' | 'other', message: string}`.
3. UI consumer (refund button component) maps `still_processing` to a yellow toast and renders Retry.
4. Add vitest case.

**【不做什么 DON'T】**
- ❌ Do NOT auto-retry on a refund — refunds touch money; admin confirms each retry.
- ❌ Do NOT surface this state as "error" — it's a transient settlement-in-progress state, expected and recoverable.
- ❌ Do NOT poll outbox endpoint from frontend; out of scope and creates load.

**【怎么做 HOW (具体)】**

```ts
// frontend/src/api/admin-payments.ts
import axios from 'axios'
import client from './client'
import { isGatewayErrorBody } from './contract/errors'

export type RefundResult =
  | { ok: true; payment: PaymentRow }
  | { ok: false; kind: 'still_processing' | 'other'; message: string }

export async function refundPayment(paymentId: string): Promise<RefundResult> {
  try {
    const res = await client.post<PaymentRow>(`/api/admin/payments/${paymentId}/refund`)
    return { ok: true, payment: res.data }
  } catch (err) {
    if (axios.isAxiosError(err) && err.response?.status === 409) {
      const body = err.response.data
      if (isGatewayErrorBody(body) && body.error_code === 'PAYMENT_STILL_PROCESSING') {
        return { ok: false, kind: 'still_processing',
                 message: 'Payment is still settling. Try again in a few seconds.' }
      }
    }
    return { ok: false, kind: 'other', message: (err as Error).message }
  }
}
```

**【如何验证 VERIFY】**
1. `pnpm vitest run src/api/__tests__/admin-payments.spec.ts -t "still_processing"` green.
2. Storybook (if present) snapshot for the Retry UI variant.
3. `make test-frontend` green.
4. Output `logs/T-WS11-09-verify.txt`.

**【依赖 + 对抗复审】**
- Depends on: T-WS11-01; WS-1 Track A1 publishes `ErrPaymentStillProcessing` with HTTP mapping (INVESTIGATE-013).
- Blocks: none.
- Adversarial review: sonnet checks (a) the Go-side `Refund.Create` actually returns the sentinel before any DB write side-effect; (b) HTTP layer maps it to 409 (not 422 / not 500); (c) the body shape matches `GatewayErrorBody`.

---

### T-WS11-10 — Wire `make test-frontend` into the WS-11 PR gate

**【是什么 What it IS】**
- A CI configuration change: ensure the PR for WS-11 (and every future PR touching `frontend/src/api/*.ts` or `tests/contract/`) runs `make test-frontend` as a required check.
- Add a `scripts/check-frontend-touched.sh` that scans the diff and fails CI if api files changed but `make test-frontend` was not run in the workflow.

**【不是什么 What it is NOT】**
- Not a frontend code change.
- Not a Go change beyond the gate script.
- Not a duplicate of `make check` — `make test-frontend` is the integration harness per CLAUDE.md.

**【为什么 Why this exists】**
- CLAUDE.md §测试与验收: "触及 api 层必跑 `make test-frontend`".
- v2 REV-006 §7 #10: "所有触及 api 层的 PR 必跑 `make test-frontend` 且通过."
- Without an automated gate, a contributor can silently skip the harness and ship an interceptor regression.

**【做什么 DO】**
1. Create `scripts/check-frontend-touched.sh`:
   - `git diff --name-only origin/main...HEAD | grep -E '^(frontend/src/api/|tests/contract/)'` → if non-empty AND env `FRONTEND_HARNESS_RAN != 1`, fail with explanatory message.
2. In `.github/workflows/ci.yml` (or whatever PR workflow exists), add a step "Run frontend harness" gated on the same grep, exporting `FRONTEND_HARNESS_RAN=1` after `make test-frontend` exits 0.
3. Add a one-line entry in `Makefile`: `check-frontend-touched: ; scripts/check-frontend-touched.sh` and chain into `check` target.

**【不做什么 DON'T】**
- ❌ Do NOT run `make test-frontend` unconditionally on every PR (expensive; uses Postgres + Redis).
- ❌ Do NOT skip the gate when `[skip ci]` is in the message — license-bearing changes are too important.

**【怎么做 HOW (具体)】**

```bash
#!/usr/bin/env bash
# scripts/check-frontend-touched.sh
set -euo pipefail
BASE="${BASE_REF:-origin/main}"
CHANGED=$(git diff --name-only "$BASE"...HEAD | grep -E '^(frontend/src/api/|tests/contract/)' || true)
if [[ -z "$CHANGED" ]]; then
  echo "frontend-touched: no api-layer changes; skipping harness gate"
  exit 0
fi
if [[ "${FRONTEND_HARNESS_RAN:-0}" != "1" ]]; then
  echo "ERROR: frontend api/contract layer changed but make test-frontend did NOT run:"
  echo "$CHANGED"
  exit 1
fi
```

**【如何验证 VERIFY】**
1. Synthetic PR touching only `frontend/src/api/me.ts` without running harness → CI fails with the explanatory message.
2. Same PR with `make test-frontend && FRONTEND_HARNESS_RAN=1 ./scripts/check-frontend-touched.sh` → passes.
3. PR touching only `modules/auth/business/service.go` → gate skipped (exit 0).
4. Output `logs/T-WS11-10-verify.txt`.

**【依赖 + 对抗复审】**
- Depends on: WS-12 (CI infra owner) coordination.
- Blocks: WS-11 final PR merge.
- Adversarial review: sonnet checks for a way to fool the env-var gate (e.g. setting `FRONTEND_HARNESS_RAN=1` manually in the workflow before running the harness). Harden by making the workflow set the variable **only after** `make test-frontend` exits 0.

---

## 3. Sequencing

```
D0:  T-WS11-01 (constants module)
D1:  T-WS11-02 verify (after WS-5 F1 lands 402) → T-WS11-03 (route contract)
D2:  T-WS11-04 (interceptor swap)    + T-WS11-06 (R7 audit, likely no-op) in parallel
D3:  T-WS11-05 (GW_AUTH_FAILED, after WS-5 F2 lands Rust constant)
     T-WS11-07 (HMAC nonce audit, likely no-op for browser)
D4:  T-WS11-08 (X-Request-Id capture)
     T-WS11-09 (PAYMENT_STILL_PROCESSING, after WS-1 A1 lands sentinel)
D5:  T-WS11-10 (CI gate) + final integration `make test-frontend`
```

Total: ~3-4 person-days as estimated in v2 §2 REV-001 table.

---

## 4. Open INVESTIGATEs (carry into v2 §99)

| ID | Question | Owner |
|---|---|---|
| INVESTIGATE-012 | License middleware: does Go actually emit 402 + `X-Reason` header as v1 §S5.1 specifies, or did the F1 implementation drift to 403/body? | T-WS11-02 |
| INVESTIGATE-013 | HTTP mapping for `ErrPaymentStillProcessing` — 409 or 422? | WS-1 A1 + T-WS11-09 |
| INVESTIGATE-014 | Exact Rust HMAC nonce header name — `X-Nonce`, `X-Signature-Nonce`, `X-Idem-Nonce`? | T-WS11-07 |
| INVESTIGATE-015 | Does any non-frontend caller in this repo (e2e harness, ops scripts) already consume the legacy six mgmt-token codes and require coordinated removal alongside T-WS11-05? | T-WS11-05 |

---

## 5. Definition of Done (PR-level)

1. All 10 tasks completed; each has a `logs/T-WS11-XX-verify.txt` artifact.
2. `pnpm tsc --noEmit && pnpm vitest run && pnpm eslint frontend/src/` — zero errors.
3. `make test-frontend` green.
4. `go test ./tests/contract/...` green; `TestLicenseWhitelistRegistered` present.
5. `git grep -E 'GW_AUTH_MGMT_TOKEN_' frontend/src/` empty.
6. `frontend/src/api/contract/errors.ts` and `modules/license/api/whitelist.go` agree (see WS-12 mirror-check script).
7. PR description links to each cited v1/v2 anchor: §S1.20, §S5.1, §S5.14, §S5.18, REV-FINAL-V13, REV-FINAL-H8.8, REV-004c, REV-004e.
8. Four INVESTIGATEs above either closed or carried with explicit followup task IDs.
9. Adversarial sonnet review completed; findings folded back into the PR.

— end of WS-11 sub-plan —
