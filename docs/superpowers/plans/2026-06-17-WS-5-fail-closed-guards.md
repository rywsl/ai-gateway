# WS-5 Fail-Closed Guards Sub-Plan (license + auth + soft-delete)

**Date:** 2026-06-17
**Scope:** S5.1 ~ S5.20 from `docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-5 + REV-FINAL-V13..V25 + REV-CROSS-AUTH from `docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §4 WS-5.
**Audience:** A senior backend engineer who **does not read Rust or Go fluently**. Every step is intended to be executable by following file paths, line numbers, literal commands, and copy-paste-able diffs.
**Predecessor docs (REQUIRED reads before any step):**
- `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-5, §5 全量矩阵
- `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §0 重要 1/重要 2, §4 WS-5 REV-FINAL-V13..V25, REV-CROSS-AUTH

---

## 0. Project-level reading rules (binding for every task in this file)

The execution agent **MUST** read these before opening any task below; they override the per-task instructions on conflict.

0.1 **Source-of-truth = the file at the cited line, not this document.** This document was authored by an LLM and the v1+v2 plans it cites were also LLM-authored. Five concrete miscites are already known:
- Original audit H9 claimed `api_keys` has no `deleted_at` column. **Actually** `migrations/00035_gateway_entity_bridge.sql:16` adds it. So H9 has **two** real defects: (a) `api_keys.deleted_at IS NULL` is missing from queries; (b) queries also fail to gate on `users.deleted_at IS NULL` via JOIN. **Both** must be fixed.
- v2 REV-FINAL-V14 told you `api_keys` has no `deleted_at`. It is wrong. Fix both (a) and (b).
- v1 §S5.11 said legacy `verify_jwt` panic on startup; v2 REV-FINAL-V19 + REV-004d softened it to feature-flag-driven N+1 / N+2 staged rollout. Use the staged version.
- v1 §S5.12 said `expected_iss` / `expected_aud` are MUST. v2 REV-FINAL-V20 downgraded these to "warn at startup; do not panic". Use the v2 wording.
- v2 REV-FINAL-V14 used "soft delete" loosely. The real semantic is "auth fail-closed across user soft-delete + apikey hard-delete-race".

0.2 **Before writing code for a task you MUST:**
- Read the cited file:line span in full (use the `Read` tool — never trust grep snippets).
- If the literal code disagrees with this plan, **stop**, write a 5-line "FIX-PLAN-DELTA" subsection inside the task, and ask the reviewer.

0.3 **v2 fixes are CANDIDATES, not contracts.** For every task you must:
- Write at least one alternative ("Plan B") in §8.
- Choose A or B and write the ADR-style reason ("A keeps fewer call-sites changed", "B avoids the lease-counter race that A reintroduces", …).
- If neither candidate matches what the verified source shows, write a Plan C grounded in the source and use it instead.

0.4 **Sectioning is mandatory; CI will refuse PRs that drop a section.** Every task has **eight** sections in order:
1. **【What it IS】** — one sentence + concrete artefact path(s)
2. **【What it is NOT】** — explicit anti-scope
3. **【Why it exists / why the bug occurs】** — root cause + audit ID
4. **【DO】** — required ordered actions
5. **【DON'T】** — anti-patterns / tempting wrong fixes
6. **【HOW (literal)】** — file paths, line numbers, diff sketches, exact commands
7. **【VERIFY】** — exact test command + literal expected output
8. **【ADR (Candidate A vs B + reason)】** — chosen path + why

0.5 **Test-first, never code-first.** For each `S5.x` task:
- Write the failing test (RED) **first** with explicit reproducer payloads.
- Land the smallest production change that turns it green (GREEN).
- Refactor without changing the test (REFACTOR).
- Then write the adversarial case (a peer attempts to bypass by negating the obvious assertion) → must also stay green.

0.6 **Feature flags are mandatory for any change that can lock customers out.** S5.1 (license enforce), S5.11 (strict_jwt_validator) and S5.21 (HMAC nonce replay_check) ship behind `enforce: enum{disabled, warn, enforce}` with `warn` default for one release.

0.7 **Cross-references inside the document.** When a step depends on another step's design landing first, the dependency is stated in §4 "DO" and the consumer step's PR must be marked `Depends-On: WS-5/Sx.y`. The CI mergebot enforces.

---

## 1. Task index

| ID | Subject | Owner track | Lang | Depends on | DoD test |
|---|---|---|---|---|---|
| S5.1  | License middleware mount order + whitelist (H7 / REV-FINAL-V13) | F1 | Go | — | e2e: bypass header → 402 |
| S5.2  | Remove e2e license bootstrap from prod (H8) | F1 | Go + CI | S5.1 | CI lint blocks prod compose with the var |
| S5.3  | sqlc soft-delete audit script + apikey JOIN fix (H9 / REV-FINAL-V14) | F1 | Go + script | — | `make audit-softdelete` exits 0 |
| S5.4  | defaultGroupID fail-closed + bootstrap seed migration (H10 / REV-FINAL-V15) | F1 | Go + migration | — | unit: ErrNoRows ≠ other err |
| S5.5  | Cache invalidator DLQ on max-attempts (D4 / REV-FINAL-V16) | F1 | Go | — | chaos: emit fail × 6 → DLQ +1 |
| S5.6  | mgmttoken Blacklist.Revoke returns err (G10) | F1 | Go | — | unit: Redis err propagates |
| S5.7  | email-verify per-target + per-IP rate-limit + 8-char base32 code (G11 / REV-004b) | F1 | Go | — | unit: 6th attempt in 30 min → 429; code is 8 chars [A-Z2-7] |
| S5.8  | apikey cache decorator: invalidate on mutation (G12 / REV-FINAL-V17) | F1 | Go | — | unit: Update → Delete called; emit fail → 503 |
| S5.9  | Logout propagates refresh-revoke err (G13) | F1 | Go | — | unit: Redis err → 503 |
| S5.10 | OAuth bind-token: Redis only, no in-mem fallback (G14 / REV-FINAL-V18) | F1 | Go | — | unit: Redis down → 503; no map state |
| S5.11 | Legacy verify_jwt → feature-flagged strict validator (R1 / REV-FINAL-V19, REV-004d) | F2 | Rust + Go config | — | unit: flag=true + None config → boot fail; flag=false → warn-only |
| S5.12 | JWT kid + static_pem fallback rules (R3 / REV-FINAL-V20) | F2 | Rust | S5.11 | unit: unknown kid + fallback off → reject |
| S5.13 | JWT allowed_algs allowlist (R13 / REV-CROSS-AUTH) | F2 | Rust | S5.11 | unit: HS256 token with RS-only allowed_algs → reject before key-family pick |
| S5.14 | HMAC nonce SETNX, opt-in (R2 / REV-FINAL-V21) | F2 | Rust + Go config | — | unit: replay within window → reject when `replay_check=nonce_redis` |
| S5.15 | trusted_proxies CIDR config + boot-time check (R4 / REV-FINAL-V22) | F2 | Rust + Go | — | unit: prod + empty proxies → boot panic |
| S5.16 | RedosTracker post-hoc + threshold = 1, check before dispatch (R14 / REV-FINAL-V5) | F2 | Rust | — | unit: first timeout disables; next call returns disabled |
| S5.17 | LedgerCounterHandler malformed DispatchOutcome (R20 / REV-FINAL-V25) | F2 | Rust | — | unit: bad event → Malformed; consumer leaves PEL |
| S5.18 | mgmt error-code unification (R25 / REV-004c) | F2 | Rust | — | unit: 4 outcomes → single HTTP code; server-log retains distinct |
| S5.19 | Sticky N=3 → random / least-loaded fallback (Routing-5 / REV-FINAL-V23) | F2 | Rust | — | unit: bound key missing → fallback; moka not written |
| S5.20 | ReservationGate cold-start = grace ∨ view (Routing-6 / REV-FINAL-V24) | F2 | Rust | — | unit: uptime<grace ∧ view=None → reject |

Two extra tasks fall under "REV-CROSS-AUTH" but are merged into S5.13 (allowed_algs) and S5.12 (kid cross-check), so the WS-5 count is **20 tasks** matching v1 §4 WS-5 (S5.1..S5.20). The Routing-6 grace+view rule and the email 8-char code are folded into S5.20 and S5.7 respectively.

---

## 2. Tasks

### S5.1 License middleware mount order + whitelist

1. **【What it IS】** A single chi middleware function `license.RequireLicensed` mounted at the web router root, plus a feature flag `[license] enforce: {disabled, warn, enforce}` (default `warn`). The mount order in `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/binaries/web/main.go` becomes `requestID → panicRecover → license → auth → authz → handler`. Artefacts: `modules/license/api/middleware.go` (new), `binaries/web/main.go` (edit), `modules/license/business/service.go` (read `Status()`).

2. **【What it is NOT】** Not a per-route guard. Not a hard kill that drops customers without warning — flag default is `warn` for one release per REV-004e. Not an IP/JWT/RBAC check; those are downstream middlewares.

3. **【Why it exists / why the bug occurs】** Audit H7 (Go, P0): handler-side calls to `license.Status()` could be skipped when individual handlers forgot to invoke them, so a single misregistered route bypassed licensing. REV-FINAL-V13 fixes mount-order; REV-004e adds the staged flag so we do not lock out paying customers on day 1.

4. **【DO】** (in order)
   a. Read `binaries/web/main.go` and list every `r.Group(...)`, `r.Mount(...)`, `chi.NewRouter()` instance. Confirm there is exactly one root router.
   b. Read `modules/license/business/service.go` to confirm `Status(ctx)` returns `(Status, error)` (struct exposing `Licensed bool`).
   c. Add `modules/license/api/middleware.go` implementing `RequireLicensed(svc *business.Service, enforce string, whitelist []string) func(http.Handler) http.Handler`.
   d. Add to `modules/license/module.go` registration so the router wiring fetches the middleware from the registry (no cross-module import).
   e. Edit `binaries/web/main.go` to mount **after** `chimiddleware.RequestID + chimiddleware.Recoverer`, **before** the auth middleware.
   f. Add config field `[license] enforce` (default `"warn"`) to `modules/license/business/authconfig.go` (or equivalent config).
   g. Write the unit test `modules/license/api/middleware_test.go` covering: (1) whitelist passes, (2) unlicensed + enforce → 402 + `X-Reason: license_required`, (3) unlicensed + warn → 200 + audit log line, (4) unlicensed + disabled → 200, (5) licensed → 200.
   h. Write the e2e test inside `tests/contract/license_middleware_e2e_test.go` that boots web binary, fakes `license.Status().Licensed=false`, and asserts 402 on `/api/wallet`.

5. **【DON'T】**
   - Don't bypass via `X-Forwarded-License` or any header trick — those become attack vectors.
   - Don't put the middleware inside a per-module `Mount`. A single root mount is the only way to catch routes new modules forget to register correctly.
   - Don't whitelist `/api/admin/license/*` — only the operator-friendly paths (`/api/license/status`, `/api/license/activate`, `/api/license/machine`); see REV-FINAL-V13 for the exact whitelist.
   - Don't ship `enforce` default = `enforce`. Default is `warn` per REV-004e. WS-15 owner flips it.

6. **【HOW (literal)】**
   Whitelist string slice **literal**:
   ```go
   var LicenseWhitelist = []string{
     "/livez",
     "/readyz",
     "/api/license/status",
     "/api/license/machine",
     "/api/license/activate",
     "/api/auth/login",
     "/api/public/", // prefix match
   }
   ```
   Pseudocode of `RequireLicensed`:
   ```go
   func RequireLicensed(svc *business.Service, mode string, whitelist []string) func(http.Handler) http.Handler {
     return func(next http.Handler) http.Handler {
       return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
         for _, p := range whitelist {
           if r.URL.Path == p || (strings.HasSuffix(p, "/") && strings.HasPrefix(r.URL.Path, p)) {
             next.ServeHTTP(w, r); return
           }
         }
         st, err := svc.Status(r.Context())
         if err != nil || !st.Licensed {
           switch mode {
           case "disabled":
             next.ServeHTTP(w, r); return
           case "warn":
             logging.AuditWarn(r.Context(), "license_required", logging.KV{"path": r.URL.Path})
             next.ServeHTTP(w, r); return
           default: // "enforce"
             w.Header().Set("X-Reason", "license_required")
             w.WriteHeader(http.StatusPaymentRequired)
             return
           }
         }
         next.ServeHTTP(w, r)
       })
     }
   }
   ```
   In `binaries/web/main.go`, the mount sequence (literal):
   ```go
   r := chi.NewRouter()
   r.Use(chimiddleware.RequestID)
   r.Use(chimiddleware.Recoverer)
   r.Use(licenseAPI.RequireLicensed(licenseSvc, cfg.License.Enforce, licenseAPI.LicenseWhitelist))
   r.Use(authAPI.Required(authSvc))          // existing auth middleware
   r.Use(authzAPI.RBAC(authzSvc))            // existing authz middleware
   // ... module routers ...
   ```

7. **【VERIFY】**
   - Unit: `go test ./modules/license/api/... -run TestRequireLicensed -count=1 -race` → all 5 sub-cases PASS.
   - Contract: `make test-frontend` → no regression.
   - e2e: `make e2e` → `TestLicenseBypass402` PASS, output line `X-Reason: license_required`.
   - Manual reproduction: `curl -i http://localhost:9090/api/wallet -H "Authorization: Bearer …"` with `Status().Licensed=false` + `enforce=enforce` → `HTTP/1.1 402 Payment Required`.

8. **【ADR — Candidate A vs B】**
   - **A (chosen): chi middleware at root** as above.
   - **B: per-module `module.Init()` injection** — each module wraps its routes.
   - **Reason A:** every audit failure stems from a *forgotten* check, so the only safe pattern is one that cannot be forgotten (root-level). B repeats the bug class.

---

### S5.2 Remove e2e license bootstrap from prod compose; runtime hard-block

1. **【What it IS】** Delete `ALM_E2E_LICENSE_BOOTSTRAP=1` from `docker-compose.prod.yml` and add a **runtime** check inside `modules/license/business/e2e_bootstrap.go` that panics if `ALM_ENV=production` is set. Plus a CI lint `scripts/lint-prod-compose.sh` that fails the build if `ALM_E2E_LICENSE_BOOTSTRAP` appears in any `*.prod.yml`.

2. **【What it is NOT】** Not a deletion of the e2e bootstrap entirely — it stays for `make e2e`. Not a rewrite of `Service.BootstrapDevLicense`. Not a JWT change.

3. **【Why it exists / why bug occurs】** H8 — a prod compose file inherited the e2e var, so prod instances self-signed a 10-year license from a hard-coded ed25519 seed. Source at `modules/license/business/e2e_bootstrap.go:13-17` (`e2eLicenseSeed` literal).

4. **【DO】**
   a. `grep -n "ALM_E2E_LICENSE_BOOTSTRAP" docker-compose*.yml` and remove every match in `*.prod.yml`.
   b. Add to top of `BootstrapDevLicense`:
      ```go
      if os.Getenv("ALM_ENV") == "production" {
        panic("e2e license bootstrap invoked in production — refusing")
      }
      ```
   c. Add `scripts/lint-prod-compose.sh` (and the PowerShell mirror per CLAUDE.md sync rule) which `grep -lE "ALM_E2E_LICENSE_BOOTSTRAP" docker-compose*.prod*.yml` and exits 1 if anything matches.
   d. Hook it into `make check`.

5. **【DON'T】**
   - Don't only fix the lint — the runtime panic is the actual guard; lint is defence in depth.
   - Don't move the literal seed into a config file — better that it's hard-coded so leaking the file makes the seed obvious in code review.

6. **【HOW (literal)】**
   - Patch `modules/license/business/e2e_bootstrap.go` line 21, insert before `seed := make(...)`.
   - Patch `Makefile` `check:` target → `check: lint-prod-compose <existing>`.
   - The lint:
     ```bash
     #!/usr/bin/env bash
     set -euo pipefail
     if grep -rEn 'ALM_E2E_LICENSE_BOOTSTRAP' docker-compose*.prod*.yml 2>/dev/null; then
       echo "ALM_E2E_LICENSE_BOOTSTRAP must not appear in production compose files" >&2
       exit 1
     fi
     ```

7. **【VERIFY】**
   - `make check` → no `ALM_E2E_LICENSE_BOOTSTRAP` lines in any prod compose; if you add one, build fails.
   - `ALM_ENV=production go run ./binaries/all-in-one` with `BootstrapDevLicense` exercised by a unit test → panic with literal message `"e2e license bootstrap invoked in production — refusing"`.

8. **【ADR】**
   - **A (chosen): runtime panic + CI lint.**
   - **B: build tag (`//go:build !production`) to exclude the file in prod builds.**
   - **Reason A:** Go build tags are slippery in containerised one-binary deployments and easy to forget for cross-compiled binaries; runtime panic always fires. CI lint catches the compose-file mistake.

---

### S5.3 sqlc soft-delete audit script + apikey JOIN fix (H9)

1. **【What it IS】** (i) A script `scripts/audit-softdelete.sh` that scans `migrations/` for tables that **own** a `deleted_at` column, then `git grep`s every `FROM <table>` and `JOIN <table>` in `modules/**/queries/*.sql`, requiring either `<table>.deleted_at IS NULL` in the WHERE/ON clause **or** a JOIN chain via a `users u … AND u.deleted_at IS NULL`. (ii) Concrete fix to `modules/apikey/model/queries/api_keys.sql` adding both `api_keys.deleted_at IS NULL` and `JOIN users u ON u.id = api_keys.user_id AND u.deleted_at IS NULL` on `GetApiKeyByID`, `GetApiKeyByHash`, `ListUserApiKeys`. Then `sqlc generate` to regenerate `modules/apikey/model/sqlcgen/api_keys.sql.go`.

2. **【What it is NOT】** Not a global ORM-style soft-delete framework. Not a change to `DeleteApiKey` (which is hard-delete, kept). Not a rewrite of the `users` table.

3. **【Why it exists / why bug occurs】** Original audit H9 stated `api_keys` had no `deleted_at` column. **WRONG** — `migrations/00035_gateway_entity_bridge.sql:13-16` ADD it (verified). REV-FINAL-V14 also got the call wrong by saying the column doesn't exist. The truth is:
   - `api_keys.deleted_at` exists and queries don't gate on it (defect (a)).
   - `users.deleted_at` exists and is not joined when resolving an api-key (defect (b)).
   - Either defect alone is enough for a tombstoned account to keep authenticating.

4. **【DO】**
   a. Re-read `modules/apikey/model/queries/api_keys.sql:60-66` (`GetApiKeyByHash`) and `:52-58` (`GetApiKeyByID`) and `:68-77` (`ListUserApiKeys`).
   b. Rewrite each to JOIN users and gate on both `deleted_at IS NULL`.
   c. Run `sqlc generate` (existing config) → regenerate `modules/apikey/model/sqlcgen/api_keys.sql.go`.
   d. Update tests in `modules/apikey/business/service_test.go` to seed a soft-deleted user and assert `not_found`.
   e. Write `scripts/audit-softdelete.sh` (+ `.ps1` mirror).
   f. Wire into `make check`.

5. **【DON'T】**
   - Don't auto-rewrite all queries with a regex — `users` joined elsewhere uses different aliases. The audit script flags **but does not edit**.
   - Don't soft-delete an api-key by tombstoning users — they are independent lifecycles. The new JOIN is read-side only.
   - Don't drop the existing `is_active` and `is_locked` flags — they are separate state and still required.

6. **【HOW (literal)】**
   `GetApiKeyByHash` (new body):
   ```sql
   -- name: GetApiKeyByHash :one
   SELECT api_keys.id, api_keys.user_id, key_hash, key_display, name, is_standalone, is_active, is_locked,
          allowed_providers, allowed_api_formats, allowed_models, force_capabilities, rate_limit,
          concurrent_limit, total_requests, total_cost_usd, last_used_at, expires_at,
          auto_delete_on_expiry, api_keys.created_at, api_keys.updated_at
   FROM api_keys
   LEFT JOIN users u ON u.id = api_keys.user_id
   WHERE api_keys.key_hash = $1
     AND api_keys.deleted_at IS NULL
     AND (api_keys.is_standalone = TRUE OR (u.id IS NOT NULL AND u.deleted_at IS NULL));
   ```
   (LEFT JOIN with the `is_standalone` shortcut keeps standalone api-keys working — they have no user row by design. **Confirm by reading `api_keys.is_standalone` semantics before applying.**)

   Audit script signature:
   ```bash
   #!/usr/bin/env bash
   set -euo pipefail
   tables=$(grep -lE 'ADD COLUMN .*deleted_at|deleted_at .*TIMESTAMP|deleted_at .*BIGINT' migrations/*.sql \
             | xargs grep -hE 'CREATE TABLE|ALTER TABLE' | awk '{print $3}' | sort -u)
   for t in $tables; do
     for q in $(git ls-files 'modules/**/queries/*.sql'); do
       if grep -qE "(FROM|JOIN) +$t( |$)" "$q"; then
         if ! grep -qE "$t\.deleted_at IS NULL|u\.deleted_at IS NULL" "$q"; then
           echo "AUDIT FAIL: $q references $t without deleted_at gate" >&2
           exit 1
         fi
       fi
     done
   done
   ```

7. **【VERIFY】**
   - `bash scripts/audit-softdelete.sh` → exit 0; intentional regression (drop the new clause) → exit 1.
   - `go test ./modules/apikey/business/... -run TestSoftDeletedUserApiKeyNotFound -race` → PASS.
   - e2e `tests/contract/api_key_soft_delete_test.go` → soft-delete user, then `GET /v1/messages` with the api-key returns 401 with code `api_key_not_found`.

8. **【ADR】**
   - **A (chosen): LEFT JOIN + standalone shortcut** (both deleted-at gates).
   - **B: WHERE EXISTS (SELECT 1 FROM users …)** — works for non-standalone only.
   - **C: enforce in business layer** (`Service.AuthByHash` calls `users.GetUser` and checks `DeletedAt`).
   - **Reason A:** the bug class is "queries forget the gate"; pushing into business layer leaks the rule across N callers (same shape as the H7 bug). A keeps the rule next to the data definition; B can't represent standalone api-keys without a CASE, so A reads cleaner.

---

### S5.4 defaultGroupID fail-closed + bootstrap seed migration (H10)

1. **【What it IS】** Rewrite `Directory.defaultGroupID` at `modules/user/business/directory.go:1433-1442` so it returns `(*string, error)` and distinguishes `pgx.ErrNoRows` from a real DB error. Callers fail closed on the latter. Plus migration `migrations/000NN_seed_default_user_group.sql` that inserts an immutable `ug_default` row, and `ALTER TABLE users ALTER COLUMN group_id SET NOT NULL` after backfill (REV-FINAL-V15).

2. **【What it is NOT】** Not a re-architecture of `Directory`. Not a change to memory store paths (only PG path). Not a change to how `is_default` is chosen.

3. **【Why it exists / why bug occurs】** H10: the current body swallows every error with `if err == nil { … } return nil`, so a transient DB blip routes users into the global "no group" pool — which has unbounded access. REV-FINAL-V15 also notes that if the row is genuinely missing, first-user signup explodes; we therefore seed `ug_default` and make `users.group_id` NOT NULL.

4. **【DO】**
   a. Read `modules/user/business/directory.go:1433-1442` literally.
   b. Change signature → `func (d *Directory) defaultGroupID(ctx context.Context) (*string, error)`.
   c. Distinguish `errors.Is(err, pgx.ErrNoRows)` → `(nil, nil)` from other err → `(nil, err)`.
   d. Update every caller. From grep: lines `957`, `971`, `1032`. Each caller must propagate err and refuse the operation if err != nil.
   e. Write migration that `INSERT INTO user_groups (id, name, is_default, …) VALUES ('ug_default', 'Default', TRUE, …) ON CONFLICT DO NOTHING;`.
   f. Add backfill `UPDATE users SET group_id = 'ug_default' WHERE group_id IS NULL;`.
   g. Then `ALTER TABLE users ALTER COLUMN group_id SET NOT NULL;`.
   h. Down migration must reverse the NOT NULL first.
   i. Unit test in `modules/user/business/directory_test.go` faking pg with `pgx.ErrNoRows` and with a random `*pgconn.PgError`.

5. **【DON'T】**
   - Don't `panic` on err — return it. Tests must observe the err shape.
   - Don't seed in `Init()` Go-code — migrations are the only ordering primitive that survives multi-replica boot.
   - Don't change the meaning of `is_default = TRUE`: only one row should hold it.

6. **【HOW (literal)】**
   New body:
   ```go
   func (d *Directory) defaultGroupID(ctx context.Context) (*string, error) {
     if d.pool == nil {
       return d.mem.defaultGroupID(), nil
     }
     var groupID string
     err := d.pool.QueryRow(ctx, `SELECT id FROM user_groups WHERE is_default LIMIT 1`).Scan(&groupID)
     if errors.Is(err, pgx.ErrNoRows) {
       return nil, nil
     }
     if err != nil {
       return nil, fmt.Errorf("defaultGroupID: %w", err)
     }
     return &groupID, nil
   }
   ```
   Caller at line `1032`:
   ```go
   gid, err := d.defaultGroupID(ctx)
   if err != nil {
     return ErrTransient // audit: log; fail-closed
   }
   if gid == nil {
     return ErrNoDefaultGroup
   }
   targetGroupID = *gid
   ```

7. **【VERIFY】**
   - `go test ./modules/user/business/... -run TestDefaultGroupID -race` → PASS for: (i) ErrNoRows → (nil, nil), (ii) random err → (nil, wrapped), (iii) row found → (ptr, nil).
   - `goose -dir migrations up` against a fresh DB then `psql -c "SELECT id FROM user_groups WHERE is_default"` → 1 row, id=`ug_default`.
   - `psql -c "\d users"` → group_id is NOT NULL.

8. **【ADR】**
   - **A (chosen): tri-state (ptr, err)** — propagates true cause.
   - **B: hard-fail on any miss** — refuses the row even when ErrNoRows happens before the seed migration lands.
   - **Reason A:** the seed migration may need a hot-deploy window where step-by-step errors must be distinguishable. A keeps that window safe. B kills the API the moment the seed is one second late.

---

### S5.5 Cache invalidator DLQ on max-attempts (D4)

1. **【What it IS】** Extend the `entity_events` cache-invalidator stream consumer to embed an `attempts` counter inside each entry's value; on the 5th retry that still fails, write the entry to `events:entity:dlq` and **then** `XACK` the main stream. New script `scripts/drain-cache-dlq.sh`. Alert when `events:entity:dlq` length > 1k. Artefacts: `binaries/web/internal/cache_invalidator.go` (or current cache invalidator file — confirm by read), `scripts/drain-cache-dlq.sh`.

2. **【What it is NOT】** Not a replacement for the supervised goroutine framework — that is WS-4. Not a Redis-Streams consumer-group rewrite.

3. **【Why it exists / why bug occurs】** D4 + REV-FINAL-V16: today on any failure path the entry is silently re-queued (or ACKed wrongly), so a poison entry can either block forever or get lost. DLQ + attempts counter resolves both.

4. **【DO】**
   a. Read the current invalidator (start: `grep -ln "events:entity" binaries/web/`). Confirm what `XACK` order looks like.
   b. Add `attempts` to the streamed JSON or a side hash field `events:entity:attempts:<entry_id>`.
   c. Pseudo: handler → on success XACK; on failure: increment attempts; if attempts ≥ 5 → XADD to `events:entity:dlq` then XACK main; else leave un-ACKed.
   d. Drain script: `XRANGE events:entity:dlq - +` → print to stdout; flag `--ack` to XDEL after print.
   e. Alert rule (Prom/Grafana) for `redis_stream_length{stream="events:entity:dlq"} > 1000`.

5. **【DON'T】**
   - Don't pin retry count in the consumer Go code by stream-internal delivery counter (XPENDING) — it resets on consumer rebalance.
   - Don't drop the `XACK` when DLQing — that's the bug.
   - Don't make `events:entity:dlq` an in-memory channel — must survive restart.

6. **【HOW (literal)】**
   ```go
   const maxAttempts = 5
   // on error
   atts, _ := r.Incr(ctx, "events:entity:attempts:"+entryID).Result()
   if atts >= maxAttempts {
     r.XAdd(ctx, &redis.XAddArgs{Stream: "events:entity:dlq", Values: map[string]any{
       "src": "events:entity", "src_id": entryID, "payload": payload, "last_err": err.Error(),
     }})
     r.Del(ctx, "events:entity:attempts:"+entryID)
     r.XAck(ctx, "events:entity", group, entryID)
   }
   ```

7. **【VERIFY】**
   - Chaos: `go test ./binaries/web/... -run TestCacheInvalidatorDLQ -race` — inject failing handler 6× → DLQ stream length +1.
   - `bash scripts/drain-cache-dlq.sh --ack` runs clean.

8. **【ADR】**
   - **A (chosen): app-side attempts counter + explicit DLQ stream.**
   - **B: Use Redis Streams' XGROUP delivery counter directly.**
   - **Reason A:** delivery counter is reset on consumer reclaim (`XAUTOCLAIM`); A is reset-safe.

---

### S5.6 mgmttoken Blacklist.Revoke returns err (G10)

1. **【What it IS】** Change `Revoke` in `modules/mgmttoken/model/blacklist.go:33-41` from `func Revoke(ctx, tokenID, ttl)` returning **nothing** to `func Revoke(ctx, tokenID, ttl) error`. Propagate the error to the caller and refuse to mark the user-facing revocation as successful unless Redis confirmed.

2. **【What it is NOT】** Not a change to the key shape (`mgmt_token:blacklist:<tokenID>`) or the TTL. Not a redesign of the Blacklist receiver.

3. **【Why it exists / why bug occurs】** Verified in source — line 40 `_ = b.r.Set(ctx, MgmtBlacklistPrefix+tokenID, "1", ttl).Err()` discards the result. Gateway probes `GET mgmt_token:blacklist:<id>` fail-CLOSED **only** when Redis is reachable; if our SET silently failed (e.g. CROSSSLOT during cluster reshard), the token continues to authorise for the full TTL. P0.

4. **【DO】**
   a. Read `blacklist.go` line 33-41.
   b. Add `error` return. Propagate up via `service.go`.
   c. Update caller (`modules/mgmttoken/business/service.go`) so the revoke API returns 503 if Redis failed.
   d. Add `module_test.go` adversarial test using `miniredis.Close()` mid-call to assert err propagates.

5. **【DON'T】**
   - Don't add a goroutine "retry-in-background"; the user has already been told the token is revoked.
   - Don't change `nil-safe (no Redis wired)` semantics to return err — the nil-receiver short-circuit is fine because in that mode there is no gateway to honour the blacklist anyway. Tests must encode this.

6. **【HOW (literal)】**
   ```go
   func (b *Blacklist) Revoke(ctx context.Context, tokenID string, ttl time.Duration) error {
     if b == nil || b.r == nil || tokenID == "" {
       return nil
     }
     if ttl <= 0 {
       return nil
     }
     if err := b.r.Set(ctx, MgmtBlacklistPrefix+tokenID, "1", ttl).Err(); err != nil {
       return fmt.Errorf("mgmttoken blacklist: %w", err)
     }
     return nil
   }
   ```
   Update `service.go` caller:
   ```go
   if err := b.Revoke(ctx, tokenID, ttl); err != nil {
     return ErrTransient // → handled as 503 at api layer
   }
   ```

7. **【VERIFY】**
   - `go test ./modules/mgmttoken/... -run TestRevokePropagatesRedisError -race` → PASS.
   - Chaos integration: kill Redis mid-`POST /api/admin/mgmt-tokens/<id>/revoke` → 503 with `code=transient_storage` and **no** body line saying "revoked".

8. **【ADR】**
   - **A (chosen): hard propagate, return 503.**
   - **B: write to outbox table, return 200 immediately.**
   - **Reason A:** mgmttoken revoke is a security operation; UX latency cost of 503 + retry is acceptable; B exposes a window of authorised use which is the original bug.

---

### S5.7 email-verify per-target + per-IP rate-limit + 8-char base32 code (G11 + REV-004b)

1. **【What it IS】** (i) Redis SETNX-counter on key `email_verify:target:<email>:bucket` with 5/30 min window. (ii) Same on key `email_verify:ip:<peer>:bucket` with 20/30 min. (iii) Replace the 6-digit decimal code with an 8-character base32 (RFC 4648 [A-Z2-7]) code generated via `crypto/rand`. (iv) Tests for both.

2. **【What it is NOT】** Not a captcha. Not a change to the email template. Not a JWT change.

3. **【Why it exists / why bug occurs】** G11 — 6-digit code is 20 bits entropy and is brute-forceable in seconds without limits. REV-004b raised to 40 bits (8 base32). The limiter alone is "band-aid"; both together are root-cause + cost-floor.

4. **【DO】**
   a. Read `modules/auth/business/verification.go` (entire file). Identify the code-generator and the issuer endpoint.
   b. Replace code generator:
      ```go
      func newCode() string {
        var b [5]byte // 5 bytes → 8 base32 chars
        _, _ = crypto_rand.Read(b[:])
        return base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(b[:])
      }
      ```
   c. Add Redis limiter:
      ```go
      func (l *Limiter) Allow(ctx context.Context, key string, max int64, window time.Duration) (bool, error) {
        n, err := l.r.Incr(ctx, key).Result()
        if err != nil { return false, err }
        if n == 1 { _ = l.r.Expire(ctx, key, window).Err() }
        return n <= max, nil
      }
      ```
   d. Wire into issuing endpoint: deny when either bucket exceeds.
   e. Adversarial test: 6 calls in 30 min for same email → 429.
   f. Property test: generated code matches `^[A-Z2-7]{8}$`.

5. **【DON'T】**
   - Don't use the `auth` module's existing IP limiter directly — it is per-route. We need per-email AND per-IP. Two different keys.
   - Don't make the code variable length — fixed 8 prevents log-grep mistakes.
   - Don't downgrade the code in dev — same generator in all envs (tests pin a seeded random under build-tag `testing`).

6. **【HOW (literal)】**
   - Patch file: `modules/auth/business/verification.go`.
   - Add `Limiter` field to the `Service` struct (or accept a `RateLimiter` interface via constructor).
   - Update `module.go` wiring to inject a `*redis.Client`-backed limiter.

7. **【VERIFY】**
   - `go test ./modules/auth/business/... -run TestVerifyCodeRateLimit -race`
     - Sub-test `Per-target` → 5 attempts PASS, 6th → `ErrRateLimited`.
     - Sub-test `Per-IP` → 20 attempts PASS, 21st → `ErrRateLimited`.
     - Sub-test `Code-Shape` → regex `^[A-Z2-7]{8}$`, 10 000 samples, all PASS, distinct count >= 9 990.
   - e2e: `make test-frontend` exercising the verify flow → 6th attempt receives 429.

8. **【ADR】**
   - **A (chosen): Redis INCR + EXPIRE.**
   - **B: leaky bucket via `golang.org/x/time/rate` in-mem.**
   - **Reason A:** B is per-instance only; behind a load balancer the attacker simply hops instances. A is centralised.

---

### S5.8 apikey cache decorator: invalidate on mutation (G12)

1. **【What it IS】** A Store decorator (`modules/apikey/business/cache_store.go`, new) that wraps the existing `Store` interface in `modules/apikey/business/`. On every successful mutation (`Update`, `Toggle`, `Delete`, `Rotate`, `UpdateCapabilities`, `RecordUsage` if it affects identity) the decorator does **both** local `cache.Delete(keyID)` **and** XADD to `events:entity` (apikey topic). If **either** side fails, the entire mutation returns `ErrCacheInvalidationFailed` mapped to HTTP 503. Per REV-FINAL-V17.

2. **【What it is NOT】** Not a change to the Service business logic. Not a change to read paths.

3. **【Why it exists / why bug occurs】** G12: today none of the mutation methods touch the in-memory cache. A revoked key remains authorised for up to 60 s. Decorator pattern prevents callers from forgetting.

4. **【DO】**
   a. Read `modules/apikey/business/service.go` and `modules/apikey/business/memory_store.go`. Identify the `Store` interface.
   b. Write the decorator wrapping `Store`.
   c. In `module.go`, replace `s.Store = pgStore` with `s.Store = NewCacheInvalidatingStore(pgStore, localCache, redisStream)`.
   d. Define error type `ErrCacheInvalidationFailed` + map at api layer to 503.
   e. Adversarial test: inject failing cache.Delete → mutation returns err and Store stays unchanged at PG level (verify by SELECT in test).

5. **【DON'T】**
   - Don't run the cache-delete in a goroutine "best-effort" — that's the current bug.
   - Don't merge the decorator into Service — the whole point is that mutation-path is centralized.
   - Don't XACK from the decorator — that's S5.5's responsibility on the consumer side.

6. **【HOW (literal)】**
   ```go
   type cacheStore struct {
     inner  Store
     cache  Cache
     stream Streamer
   }

   func (c *cacheStore) Update(ctx context.Context, id string, p UpdateParams) (ApiKey, error) {
     k, err := c.inner.Update(ctx, id, p); if err != nil { return k, err }
     if err := c.cache.Delete(id); err != nil { return k, fmt.Errorf("cache delete: %w", err) }
     if err := c.stream.Emit(ctx, "apikey", id, "mutate"); err != nil { return k, fmt.Errorf("emit: %w", err) }
     return k, nil
   }
   // mirror for Toggle, Delete, Rotate, UpdateCapabilities
   ```
   Map `ErrCacheInvalidationFailed` to 503 in `api/routes.go`.

7. **【VERIFY】**
   - `go test ./modules/apikey/business/... -run TestCacheInvalidatingStore -race` covers: (a) Update success → cache.Delete called once + stream.Emit called once; (b) cache fail → err returned; (c) stream fail → err returned; (d) PG fail → cache untouched, stream untouched.
   - Contract: `tests/contract` route returns 503 when injected.

8. **【ADR】**
   - **A (chosen): decorator at Store boundary.**
   - **B: extra method `Service.InvalidateAfter(mutation func)`.**
   - **Reason A:** B requires every Service method to call it, exactly the failure mode of G12. A is impossible-to-forget.

---

### S5.9 Logout propagates refresh-revoke err (G13)

1. **【What it IS】** In `modules/auth/api/routes.go`, the `/api/auth/logout` handler must check the error from `svc.RevokeRefresh(...)` and return 503 if Redis failed. Today (per audit) it does `_ = svc.RevokeRefresh(...)`. Front-end maintains a retry banner (UI change is out of WS-5; document the contract here).

2. **【What it is NOT】** Not a redesign of refresh tokens. Not a change to access-token rotation.

3. **【Why it exists / why bug occurs】** G13 — silently dropping the error means the refresh JTI lives on in Redis with TTL, so an attacker who stole the refresh token continues to mint access tokens for hours.

4. **【DO】**
   a. Read `modules/auth/api/routes.go`. Locate logout handler.
   b. Capture err, distinguish three cases: token missing (200), revoke success (200), Redis err (503 with `code: transient_storage_logout_failed`).
   c. Add a contract assertion in `tests/contract/auth_logout_test.go` requiring the 503 code path exists.

5. **【DON'T】**
   - Don't clear the access-token cookie before the revoke succeeds — user thinks they logged out when they didn't.
   - Don't auto-retry inside the handler — front-end retries are clearer.

6. **【HOW (literal)】**
   ```go
   if err := svc.RevokeRefresh(ctx, jti); err != nil {
     httpwrap.Error(w, http.StatusServiceUnavailable, "transient_storage_logout_failed", err)
     return
   }
   ```

7. **【VERIFY】**
   - `go test ./modules/auth/api/... -run TestLogoutPropagatesError -race` PASS for both branches.
   - Chaos: `miniredis.Close()` → 503.

8. **【ADR】**
   - **A (chosen): hard-503.**
   - **B: queue revoke to outbox.**
   - **Reason A:** logout is not an offline-tolerant op. B introduces the very window we're closing.

---

### S5.10 OAuth bind-token: Redis only, no in-mem fallback (G14)

1. **【What it IS】** Move OAuth bind-token storage in `modules/oauth/business/service.go` to a Redis HSET with 5-min TTL on key `oauth:bind:<state>`. Remove the in-memory map. If Redis is unreachable on read or write → 503 with `code: oauth_bind_storage_unavailable`. Per REV-FINAL-V18.

2. **【What it is NOT】** Not a change to the OAuth provider list or to the state nonce shape.

3. **【Why it exists / why bug occurs】** G14: in-mem map = no janitor + non-shared across replicas → callback hits a different pod than the start endpoint and "loses" the binding, or worse, a stale entry survives forever.

4. **【DO】**
   a. Read `modules/oauth/business/service.go` and find the map.
   b. Replace with `r.HSet(ctx, "oauth:bind:"+state, "user_id", uid, "intent", intent, "created_at", now)` + `r.Expire(ctx, key, 5*time.Minute)`.
   c. Callback reads HGETALL; if missing → 400 `state_expired_or_unknown`. If `HGetAll` returns err → 503.
   d. Delete map field from struct (compile-time guarantee of removal).
   e. Tests: (i) success round-trip, (ii) Redis err on write → 503, (iii) Redis err on read → 503, (iv) expiry → 400.

5. **【DON'T】**
   - Don't fall back to in-mem on Redis failure — that re-introduces G14.
   - Don't use a Postgres table — bind tokens are short-lived; Postgres pollutes WAL.

6. **【HOW (literal)】**
   ```go
   func (s *Service) PutBind(ctx context.Context, state string, b Bind) error {
     err := s.r.HSet(ctx, "oauth:bind:"+state, map[string]any{
       "user_id":    b.UserID,
       "intent":     b.Intent,
       "created_at": b.CreatedAt.Unix(),
     }).Err()
     if err != nil { return err }
     return s.r.Expire(ctx, "oauth:bind:"+state, 5*time.Minute).Err()
   }
   ```

7. **【VERIFY】**
   - `go test ./modules/oauth/business/... -run TestBindNoInMemFallback -race`.
   - Verify map field is gone with `grep -n "map\[string\]" modules/oauth/business/service.go` → zero map fields of bind shape.

8. **【ADR】**
   - **A (chosen): Redis HSET.**
   - **B: PG `oauth_binds` table.**
   - **Reason A:** sub-second TTL semantics and inter-replica reach. B requires a partition-on-time table to avoid bloat — overkill.

---

### S5.11 Legacy verify_jwt → feature-flagged strict validator (R1)

1. **【What it IS】** In the Rust gateway crate `crates/gw-auth/src/jwt.rs`, gate the legacy `verify_jwt` path behind a new `JwtValidatorConfig.strict: bool` (Go ↔ Rust contract addition). With `strict = false` (default for one release): warn-and-proceed on missing iss/aud/nbf. With `strict = true`: refuse boot if `cfg.global.jwt_validator.is_none()`. With `strict = true` AND `jwt_validator.is_some()`: enforce iss/aud/nbf strictly. Per REV-FINAL-V19 + REV-004d.

2. **【What it is NOT】** Not an immediate panic-on-boot. Not a deletion of the legacy code path in this PR — that lands two releases later per REV-004d.

3. **【Why it exists / why bug occurs】** R1: today the default code path only checks `exp`. iss/aud/nbf missing → any token signed by *any* trusted issuer is accepted as if it were ours = cross-tenant impersonation, P0.

4. **【DO】**
   a. Read `crates/gw-auth/src/jwt.rs`. Confirm name `verify_jwt` and the structure of `JwtValidatorConfig`.
   b. Add `pub strict: bool` field, serde default `false`.
   c. On boot, if `strict && cfg.jwt_validator.is_none()` → log error + return `Err(BootError::JwtValidatorRequired)` (do NOT panic — boot path should be testable).
   d. Inside `verify_jwt`, if `strict`, require iss/aud/nbf, otherwise emit `tracing::warn!` once-per-key.
   e. Sync Go contract: `shared/rustpact` adds the `strict` knob; default false.
   f. WS-15 owner schedules the flip.

5. **【DON'T】**
   - Don't call `panic!` on the boot path (kills the supervisor; harder to roll back).
   - Don't keep two code branches forever — REV-004d schedules deletion at N+2.

6. **【HOW (literal)】**
   - Sketch:
     ```rust
     #[derive(Deserialize, Default)]
     pub struct JwtValidatorConfig {
       #[serde(default)]
       pub strict: bool,
       pub allowed_algs: Vec<Algorithm>, // see S5.13
       pub expected_iss: Option<String>,
       pub expected_aud: Option<String>,
       pub require_nbf: bool,
       // … existing …
     }

     pub fn boot_check(cfg: &GlobalConfig) -> Result<(), BootError> {
       if cfg.jwt_validator.strict && cfg.jwt_validator_overrides.is_none() {
         return Err(BootError::JwtValidatorRequired);
       }
       Ok(())
     }
     ```

7. **【VERIFY】**
   - `cargo test -p gw-auth jwt_validator_strict_flag` → cases (i) strict=false + None config → boot OK + warn log; (ii) strict=true + None → boot fail with `BootError::JwtValidatorRequired`; (iii) strict=true + Some + iss/aud/nbf present → token OK; (iv) strict=true + token missing nbf → reject.
   - Contract sync: `scripts/sync-rust-contract.sh` shows the new field; Go test `TestStrictJwtConfigRoundTrip` passes.

8. **【ADR】**
   - **A (chosen): feature flag + N+1 default flip + N+2 delete.**
   - **B: hard panic now.**
   - **Reason A:** REV-004d explicitly rejected B because every customer currently runs default (None) and would brick. A gives a release of warning telemetry + a release of actual enforcement.

---

### S5.12 JWT kid + static_pem fallback rules (R3)

1. **【What it IS】** In `crates/gw-auth/src/jwt.rs`, refuse unknown `kid` outright **unless** `static_pem_rs256` is configured AND `require_nbf` is set. Validation of (iss, aud) is recommended; we **warn at startup** rather than panic per REV-FINAL-V20. Kid match strategy: configure a `kid_table` mapping kid → key family; absence → reject.

2. **【What it is NOT】** Not a JWKS rewrite. Not a key-rotation policy.

3. **【Why it exists / why bug occurs】** R3: today an unknown `kid` silently falls back to `static_pem_rs256`. A stolen static_pem private key forges any tenant. P0.

4. **【DO】**
   a. Read existing kid resolution in `crates/gw-auth/src/jwt.rs`.
   b. Add `kid_table: HashMap<String, KeyFamily>` to config.
   c. Resolution algorithm: if `header.kid in kid_table` use it; else if `static_pem_rs256.is_some() && static_pem_rs256.require_nbf` use it; else `return Err(Unknown_Kid)`.
   d. Startup warn (not panic) if `static_pem_rs256.is_some() && expected_iss.is_none()`.
   e. Tests for all four branches.

5. **【DON'T】**
   - Don't make iss/aud mandatory to enable static_pem — REV-FINAL-V20 says warn-not-panic.
   - Don't lookup `kid` in JWKS list **before** kid_table — that defeats the cross-check.

6. **【HOW (literal)】** see above pseudocode.

7. **【VERIFY】**
   - `cargo test -p gw-auth jwt_kid_resolution` → 4 cases.
   - Startup with `static_pem_rs256.is_some()` + no `expected_iss` → log line `kid_static_pem_no_iss_warn` (assert via `tracing-test`).

8. **【ADR】**
   - **A (chosen): kid_table + nbf-gated static_pem fallback.**
   - **B: JWKS-only, no static_pem.**
   - **Reason A:** existing fleet still needs static_pem for some private deployments; A keeps them on the safer side while B requires a JWKS server everywhere (out of scope for this WS).

---

### S5.13 JWT allowed_algs allowlist (R13 / REV-CROSS-AUTH)

1. **【What it IS】** Add `JwtValidatorConfig.allowed_algs: Vec<Algorithm>` (Rust) and mirror in `shared/rustpact` (Go). Boot-time: empty list → `BootError`. Token verification: extract `header.alg` **first**, reject if not in `allowed_algs`, **then** pick the key family. Per REV-CROSS-AUTH.

2. **【What it is NOT】** Not a one-alg-only restriction; we may run hybrid HS+RS during rotation.

3. **【Why it exists / why bug occurs】** R13: today the validator picks key family by `header.alg`. A token forger sets `alg: HS256` and signs with the *public* key of the RS256 key family. Classic alg confusion. P0.

4. **【DO】**
   a. Read `crates/gw-auth/src/jwt.rs` resolution logic.
   b. Add `allowed_algs: Vec<Algorithm>` (default empty → boot reject).
   c. Refactor so `verify_jwt` does: parse header → check `allowed_algs.contains(&header.alg)` → reject if not → only then pick key family.
   d. Adversarial test: RS-only allowed_algs + HS256 token → reject **even if** the HS256 key family is configured.

5. **【DON'T】**
   - Don't allow `Algorithm::Hs256` and `Algorithm::Rs256` both *implicitly* when migration mid-flight. Operators must list explicitly. Boot rejects empty list to make the gap loud.
   - Don't put the check after key-family selection — order matters.

6. **【HOW (literal)】** see S5.11 sketch.

7. **【VERIFY】**
   - `cargo test -p gw-auth allowed_algs` → (i) empty list → boot rejects, (ii) HS256 token, only `[Rs256]` allowed → reject before key family lookup, (iii) HS256 token, `[Hs256, Rs256]` allowed → accepted by HS family.

8. **【ADR】**
   - **A (chosen): explicit allowlist, boot-reject on empty.**
   - **B: implicit allowlist = whichever family is configured.**
   - **Reason A:** B is the current shape, and the source of R13. A makes the implicit explicit.

---

### S5.14 HMAC nonce SETNX, opt-in (R2)

1. **【What it IS】** Add config `signature_secret.replay_check: enum{none, nonce_redis, nonce_local}`, default `none`. When `nonce_redis`, on every signed request the validator must `SETNX sig_nonce:<nonce> 1 EX 600`; failure to set (key existed) → reject. Also shrink the `±300s` clock window to `±60s` as cheap defence in depth.

2. **【What it is NOT】** Not a mandatory rollout; operators may stay on `none` if they accept the risk. Not a JWT change.

3. **【Why it exists / why bug occurs】** R2 — current ±300 s window without nonce ⇒ 5-minute reuse window. Combined with R4 (no real IP) this lets an attacker replay continuously.

4. **【DO】**
   a. Read the HMAC verifier in `crates/gw-auth/src/` (locate `nonce` references).
   b. Add config knob.
   c. Implement `nonce_redis`: `SETNX sig_nonce:{nonce} 1 EX 600`; on `EXISTS` → `Err(ReplayDetected)`.
   d. Implement `nonce_local`: in-memory expiring cache (moka with 10 min TTL) — explicitly documented as single-instance-only.
   e. Shrink default window to ±60 s.
   f. Tests for each mode.

5. **【DON'T】**
   - Don't make nonce required globally before WS-15 finalises rollout — replay-vulnerable customers explicitly opt in.
   - Don't fall back to `nonce_local` when Redis fails — fail-closed by operator choice: 503.

6. **【HOW (literal)】**
   ```rust
   pub enum ReplayCheck { None, NonceRedis, NonceLocal }

   async fn check_replay(&self, nonce: &str) -> Result<(), AuthError> {
     match self.cfg.replay_check {
       ReplayCheck::None => Ok(()),
       ReplayCheck::NonceRedis => {
         let key = format!("sig_nonce:{nonce}");
         let set = self.redis.set_nx_ex(&key, "1", 600).await
           .map_err(AuthError::ReplayStoreUnavailable)?;
         if !set { return Err(AuthError::ReplayDetected); }
         Ok(())
       }
       ReplayCheck::NonceLocal => { /* moka with TTL 600 */ Ok(()) }
     }
   }
   ```

7. **【VERIFY】**
   - `cargo test -p gw-auth hmac_replay` covering each enum branch.
   - chaos: kill Redis with mode=NonceRedis → 503, no fallback.

8. **【ADR】**
   - **A (chosen): explicit enum + nonce_redis as primary.**
   - **B: nonce always-on with Redis required.**
   - **Reason A:** B locks out customers that haven't deployed Redis-for-auth yet. A respects deployment heterogeneity while making the safe choice ergonomic.

---

### S5.15 trusted_proxies CIDR + boot-time check (R4)

1. **【What it IS】** Add config `[runtime] trusted_proxies = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]`. `client_ip_from_session` (Rust) reads `X-Forwarded-For` ONLY when `peer_addr` ∈ trusted CIDR; otherwise uses `peer_addr`. Boot-time: `GW_ENV=production` AND `trusted_proxies=[]` → `BootError::ProductionTrustedProxiesEmpty`. Provide GCP and AWS LB CIDR templates in `docs/runbooks/trusted_proxies_templates.md`. Per REV-FINAL-V22. Go side reads the same config via `shared/rustpact`.

2. **【What it is NOT】** Not a config-file rewrite. Not a per-route limiter change.

3. **【Why it exists / why bug occurs】** R4 — attackers spoof XFF and bypass IP-based rate limits + audit. P0.

4. **【DO】**
   a. Read `crates/gw-auth/src/ratelimit/` for `client_ip_from_session`.
   b. Introduce CIDR parsing (use `ipnet` crate already in deps or add).
   c. Add boot check.
   d. Document templates.
   e. Sync to Go side: same field on Go middleware (H12 owner already aligned in §S7.2 of v1).

5. **【DON'T】**
   - Don't accept `0.0.0.0/0` in trusted_proxies — boot warn (or reject if `GW_ENV=production`).
   - Don't use `peer_addr.is_loopback()` as a shortcut — that misses sidecar topologies.

6. **【HOW (literal)】**
   ```rust
   pub fn client_ip(peer: SocketAddr, xff: Option<&str>, trusted: &[IpNet]) -> IpAddr {
     if trusted.iter().any(|n| n.contains(&peer.ip())) {
       if let Some(h) = xff { /* parse left-most public */ }
     }
     peer.ip()
   }
   ```

7. **【VERIFY】**
   - `cargo test -p gw-auth trusted_proxies` 4 cases.
   - `GW_ENV=production cargo run --bin gateway` with empty trusted_proxies → exits 2 with literal message `production_trusted_proxies_empty`.

8. **【ADR】**
   - **A (chosen): explicit CIDR list + boot-fail in prod.**
   - **B: trust XFF when peer is RFC1918.**
   - **Reason A:** RFC1918 ≠ trusted (the sidecar might be RFC1918 and untrusted). A is strict by intent.

---

### S5.16 RedosTracker post-hoc + threshold 1, check before dispatch (R14 / REV-FINAL-V5)

1. **【What it IS】** Modify `RedosTracker` in `crates/gw-auth/src/model_permissions.rs` so `is_disabled` is checked **before** `check_model_against_permissions`. Threshold lowered from 10 → 1: first timeout disables. Drop the "static estimation" idea — `regex` is RE2 (linear), estimation is meaningless.

2. **【What it is NOT】** Not a switch away from `regex` crate. Not a per-pattern fix.

3. **【Why it exists / why bug occurs】** R14 — today the budget check runs **after** the match attempt, so the first 10 slow regex attempts run un-budgeted. REV-FINAL-V5 confirmed RE2 means a static-cost estimator is misguided; post-hoc with threshold 1 is the simple correct answer.

4. **【DO】**
   a. Read `crates/gw-auth/src/model_permissions.rs:130-180` (`RedosTracker` impl) and `crates/gw-auth/src/model_permissions.rs:220-260` (`check_with_budget`).
   b. Add `is_disabled(key) -> bool` invoked **before** dispatch.
   c. Change `TIMEOUT_THRESHOLD: u32 = 10` → `1`.
   d. Tests: 1 timeout, immediate disable; second call returns disabled error.

5. **【DON'T】**
   - Don't try to estimate regex complexity.
   - Don't reset the tracker on success (we want disabled-forever-until-config-change semantics).

6. **【HOW (literal)】**
   ```rust
   pub const TIMEOUT_THRESHOLD: u32 = 1;

   pub fn check_with_budget(
       key: KeyId,
       tracker: &RedosTracker,
       ...
   ) -> Result<Vec<ModelPermission>, RedosDisabled> {
       if tracker.is_disabled(key) {
           return Err(RedosDisabled);
       }
       // existing check ...
   }
   ```

7. **【VERIFY】**
   - `cargo test -p gw-auth redos_tracker` → first timeout disables; second call short-circuits without running the regex.

8. **【ADR】**
   - **A (chosen): post-hoc, threshold 1.**
   - **B: pre-hoc complexity estimator.**
   - **Reason A:** REV-FINAL-V5 ruled B impossible (RE2 linear).

---

### S5.17 LedgerCounterHandler malformed → DispatchOutcome::Malformed (R20 / REV-FINAL-V25)

1. **【What it IS】** Extend the `DispatchOutcome` enum in the Rust counter handler with a new variant `Malformed { entry_id: String, why: &'static str }`. `LedgerCounterHandler` returns `Malformed` when the `lease_credits` field is missing or non-int. The consumer treats `Malformed` as "do not XACK; leave in PEL for XAUTOCLAIM" + emits `gw_counter_malformed_total{why}` metric.

2. **【What it is NOT】** Not a dead-letter promotion at the same time. Just refuse to overwrite credits with 0.

3. **【Why it exists / why bug occurs】** R20 — current code defaults missing/garbage `lease_credits` to 0 and writes that into `counter:`, locking the user out. P0.

4. **【DO】**
   a. Read the counter handler (locate by `grep -ln "lease_credits" crates/gw-ledger`).
   b. Add `Malformed` variant.
   c. Map parse failures to it.
   d. Consumer: match-skip XACK; metric increment.

5. **【DON'T】**
   - Don't silently retry — that recreates the bug if the entry is permanently malformed.
   - Don't write `counter:` at all on Malformed.

6. **【HOW (literal)】**
   ```rust
   pub enum DispatchOutcome { Ok(u64), Skip, Malformed(&'static str) }
   match parse_lease_credits(&entry) {
     Some(v) if v >= 0 => DispatchOutcome::Ok(apply(...)),
     _ => DispatchOutcome::Malformed("lease_credits_missing_or_negative"),
   }
   ```
   Consumer:
   ```rust
   match handler.dispatch(entry) {
     DispatchOutcome::Ok(_) => xack(...),
     DispatchOutcome::Skip => xack(...),
     DispatchOutcome::Malformed(why) => {
       metrics::counter!("gw_counter_malformed_total", "why" => why).increment(1);
       // leave PEL, XAUTOCLAIM will retry; eventually DLQ via S5.5-equivalent
     }
   }
   ```

7. **【VERIFY】**
   - `cargo test -p gw-ledger counter_handler_malformed` → metric increments; XACK not called.
   - Re-feed correct event → consumer recovers via XAUTOCLAIM.

8. **【ADR】**
   - **A (chosen): explicit Malformed variant + leave PEL.**
   - **B: drop to DLQ immediately.**
   - **Reason A:** transient parser bugs (e.g. mid-deploy schema mismatch) can self-heal via XAUTOCLAIM. B's eager DLQ loses recovery.

---

### S5.18 mgmt error-code unification (R25 / REV-004c)

1. **【What it IS】** `verify_mgmt_token_secret` today returns 4 distinct HTTP outcomes (`GW_AUTH_MGMT_TOKEN_*_*`). Collapse them to a single user-facing `GW_AUTH_FAILED`. Server-side log retains the distinct outcome via `tracing::warn!`. Per REV-004c.

2. **【What it is NOT】** Not a change to the actual verification logic.

3. **【Why it exists / why bug occurs】** R25 — distinct error codes leak which check failed → status oracle for attackers.

4. **【DO】**
   a. Read `verify_mgmt_token_secret` body.
   b. Define `MgmtAuthOutcome` enum (internal); always map to public `GW_AUTH_FAILED`.
   c. Log enum at INFO/WARN with field `outcome=…`.
   d. Adversarial test: 4 outcomes → identical body bytes.

5. **【DON'T】**
   - Don't drop the distinction in logs — operators need it.

6. **【HOW (literal)】**
   ```rust
   fn map_public(_internal: MgmtAuthOutcome) -> ApiError {
     ApiError::new("GW_AUTH_FAILED", StatusCode::UNAUTHORIZED)
   }
   ```

7. **【VERIFY】**
   - `cargo test -p gw-auth mgmt_error_unification` 4 sub-cases compare response body bytes — must be byte-identical.

8. **【ADR】**
   - **A (chosen): public-collapse, server-log-distinct.**
   - **B: keep distinct codes.**
   - **Reason A:** REV-004c rules B out (status oracle). A passes the same info to ops via logs.

---

### S5.19 Sticky: random / least-loaded fallback (Routing-5 / REV-FINAL-V23)

1. **【What it IS】** Remove the "after N=3 unavailable, rewrite moka" logic from `crates/gw-routing/src/strategy/sticky.rs`. When the bound key is unavailable, the **current** request falls back to `random available` or `least loaded available`; moka is NOT updated. No counter, no state. Per REV-FINAL-V23.

2. **【What it is NOT】** Not a removal of stickiness entirely — bound key still wins when available.

3. **【Why it exists / why bug occurs】** Routing-5 — current N=3 design creates a race where one bad backfill rewrites moka for every entrypoint. REV-FINAL-V23 ruled it "too complex; remove".

4. **【DO】**
   a. Read `crates/gw-routing/src/strategy/sticky.rs`.
   b. Remove "after N consecutive unavailable" branch.
   c. Replace with on-the-fly fallback that does NOT write moka.
   d. Tests: bound-key-missing → current request uses random; moka unchanged.

5. **【DON'T】**
   - Don't preserve any rebind counter — REV-FINAL-V23 says simple.

6. **【HOW (literal)】**
   ```rust
   fn pick(&self, candidates: &[Provider], bound: &Option<KeyId>) -> Option<Provider> {
     if let Some(k) = bound {
       if let Some(p) = candidates.iter().find(|p| p.key_id == *k && p.available) {
         return Some(p.clone());
       }
     }
     // current request only
     candidates.iter().filter(|p| p.available).choose(&mut rand::thread_rng()).cloned()
   }
   ```

7. **【VERIFY】**
   - `cargo test -p gw-routing sticky_fallback` → moka write_count == 0 across 100 fallback picks.

8. **【ADR】**
   - **A (chosen): pure fallback, no rebind.**
   - **B: rebind after N consecutive.**
   - **Reason A:** simpler, no race, no consensus needed across concurrent requests.

---

### S5.20 ReservationGate cold-start = grace ∨ view (Routing-6 / REV-FINAL-V24)

1. **【What it IS】** Modify `crates/gw-routing/src/reservation_gate.rs` so bootstrap-complete is `gateway_uptime > startup_grace_secs (30s) OR ReservationGlobalView.last_event_received_at.is_some()`. Until either is true, the gate **rejects** the request (fail-closed).

2. **【What it is NOT】** Not a redesign of the reservation stream.

3. **【Why it exists / why bug occurs】** Routing-6 — current code lets requests through during cold-start because `view == None` is interpreted as "no constraints yet". Cross-instance double-spend follows.

4. **【DO】**
   a. Read `reservation_gate.rs`.
   b. Add a `startup_grace_secs: u64` config (default 30).
   c. Track `gateway_started_at: Instant`.
   d. Replace gate logic with the `OR` rule.
   e. Adversarial test: uptime 5 s, view None → reject; uptime 35 s → accept; view Some → accept regardless of uptime.

5. **【DON'T】**
   - Don't accept during grace just because uptime is "low and harmless" — that's the bug.

6. **【HOW (literal)】**
   ```rust
   pub fn is_bootstrap_complete(&self) -> bool {
     self.gateway_started_at.elapsed() > Duration::from_secs(self.cfg.startup_grace_secs)
         || self.view.last_event_received_at().is_some()
   }
   pub fn allow(&self, ...) -> Result<(), GateRejected> {
     if !self.is_bootstrap_complete() { return Err(GateRejected::ColdStart); }
     // existing reservation logic
   }
   ```

7. **【VERIFY】**
   - `cargo test -p gw-routing reservation_gate_cold_start` covers the 3 scenarios above.

8. **【ADR】**
   - **A (chosen): grace ∨ view.**
   - **B: grace ∧ view.**
   - **Reason A:** B starves cold restarts forever in low-traffic clusters where no reservation event arrives. A bounds the wait.

---

## 3. DoD aggregate (mirrors v1 §4 WS-5 DoD + v2 additions)

- **e2e**: bypass-license → 402; soft-delete user → apikey 401; HMAC replay-within-window → 401 (`replay_check=nonce_redis`); spoof XFF → rate-limit unchanged (peer_addr counted).
- **Unit**: JWT alg not in `allowed_algs` → reject; unknown kid + fallback off → reject; `defaultGroupID` distinguishes ErrNoRows from other err; `RedosTracker` disables on first timeout; `DispatchOutcome::Malformed` path; mgmt-token 4 outcomes byte-identical public response.
- **Chaos**: Redis mid-call to mgmttoken Revoke → 503 (S5.6); Redis down for OAuth bind → 503 (S5.10); Logout Redis down → 503 (S5.9); cache-invalidator fail × 6 → DLQ +1 (S5.5).
- **Config-time**: `GW_ENV=production` + empty `trusted_proxies` → boot reject (S5.15); strict_jwt_validator=true + None config → boot reject (S5.11); empty `allowed_algs` → boot reject (S5.13).
- **Static**: `make audit-softdelete` exits 0; `make check` blocks any prod compose containing `ALM_E2E_LICENSE_BOOTSTRAP`.

---

## 4. Open INVESTIGATEs (raise to v2 §99 if you confirm)

- **INVESTIGATE-WS5-01**: Confirm `api_keys.is_standalone = TRUE` rows truly have no user link before applying the `LEFT JOIN + standalone shortcut` of S5.3. If they do have a `user_id`, the JOIN needs an inner join with `OR users IS NULL` carve-out.
- **INVESTIGATE-WS5-02**: Confirm `RegisterLicense` / `LicenseMiddleware` is not already mounted elsewhere in `binaries/all-in-one/main.go` to avoid double-mount that converts the warn-log into a duplicate warn (cosmetic but noisy).
- **INVESTIGATE-WS5-03**: Confirm `ReservationGate.allow` is not called for the same request twice. If it is, the rejection counter will double — needs to be idempotent per request.
- **INVESTIGATE-WS5-04**: Verify `signature_secret.replay_check = nonce_redis` does not conflict with the HMAC code paths used by Aether tunnel compat (cross-ref to `tunnel-compat-cnexus-aether` MEMORY entry).
- **INVESTIGATE-WS5-05**: Some `modules/auth/business/verification.go` flows may already pull a 4-digit OTP from external SMS — confirm the 8-char base32 change is restricted to **email** verification, not SMS.

---

## 5. Cross-WS dependencies

- **WS-11 (front-end)** must learn the new 402 (S5.1), 503 (S5.6, S5.9, S5.10), `GW_AUTH_FAILED` (S5.18), and the 8-char code format (S5.7). Land WS-11 ticket the same day this PR opens.
- **WS-12 (CI / build)** owns `lint-prod-compose.sh` (S5.2), `audit-softdelete.sh` (S5.3), `metric label allowlist` (REV-FINAL-V7) — list them in WS-12's plan.
- **WS-14 (migrations)** assigns the goose number for the `seed default user group` migration in S5.4.
- **WS-15 (feature flags)** owns the flip of `license.enforce: warn → enforce` and `jwt_validator.strict: false → true`. The PR landing this file does NOT flip.
- **WS-16 (security disclosure)** owns customer comms for R1/R2/R3/R4 (S5.11, S5.14, S5.12, S5.15).

---

## 6. Mirror

This file is mirrored to `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-5-fail-closed-guards.md`. Update both in lockstep.

---

## 7. Sign-off checklist (before opening the PR)

- [ ] Every section 1-8 present for every S5.x.
- [ ] Every cited file path opened via `Read` and confirmed to match.
- [ ] Every "candidate fix" has a Plan B + ADR.
- [ ] Every test command in §VERIFY produces literal expected output captured in the test name or the assertion.
- [ ] `make check` green.
- [ ] `make e2e` green.
- [ ] WS-11/12/14/15/16 hand-off tickets opened.
