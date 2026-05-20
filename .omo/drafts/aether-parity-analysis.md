# Draft: Aether → ailinkmax-gateway Feature Parity Analysis

## Requirements (confirmed)

- **Goal**: Plan a *feature parity roadmap* — produce a gap analysis that lists what the Rust gateway is missing vs Aether (canonical source).
- **Aether is canonical**: every Aether capability is a candidate gap unless the Rust gateway already covers it.
- **Audience**: Internal eng team — drives the roadmap to align Rust gateway functionality with Aether.
- **Output structure**: BOTH capability-area narrative + flat structured gap catalog (Feature | Aether location | Rust status | Effort | Priority | Notes).
- **Stratification**: Each gap MUST be tagged as either:
  - `rust-proxy` — belongs in the Rust data plane (`ailinkmax-gateway`)
  - `go-web` — belongs in the future Go control-plane / web layer
  - `worker` — already covered (in part) by the async worker design in `docs/superpowers/specs/2026-05-19-task-worker-design.md`
  - `shared` — needs both
- **Method**: Docs-first (CLAUDE.md, gateway specs/ADRs, Aether docs/architecture/, Aether README), source verification where docs are ambiguous.

## Technical Decisions

- **Plan type**: This is a *research + writing* plan, not an implementation plan. Tasks dispatch explore/librarian/oracle agents to map both codebases and produce the gap catalog. No source code is written.
- **Single deliverable**: One markdown report at `docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md` (the file lives in the gateway repo so the team can act on it).
- **Verification**: Agent-executed via file-existence + content checks (table row count, mandatory sections, all gaps tagged with stratum) and a `deep` review against both codebases for missed Aether features.
- **No test infrastructure question**: Pure analysis deliverable; no code = no tests. QA scenarios verify the *report* (sections present, every gap stratified, every Aether subsystem covered).

## Research Findings (will be populated by agents)

### Aether layout (initial scan)
- `src/` — Python gateway (FastAPI, services for auth/billing/routing/rate-limit/payment/usage/scheduling/orchestration/proxy_node/wallet/subscription/license/management_token/notification/oauth/ldap/gemini_files).
- `src/api/` — REST endpoints (admin, user_me, wallet, payment, subscription, monitoring, analytics, announcements, oauth, public, internal).
- `aether-proxy/` — Rust forward proxy node (TUI setup, DNS cache, connection pool, target filtering, registration, tunnel).
- `aether-hub/` — Rust control plane (proxy_conn, local_relay, protocol).
- `frontend/` — Admin UI.
- `alembic/` — DB migrations (Postgres).

### Rust gateway layout (initial scan)
- `gw-core` — IR, IDs, error codes (types only).
- `gw-codec` (+ macros) — OpenAI/Anthropic/Gemini codec.
- `gw-store` — Postgres + EntityStore + KMS.
- `gw-auth` — 5-tier permission merge (ADR-023), API key pair auth.
- `gw-routing` — 8 candidate strategies + filtering.
- `gw-ledger` — Redis stream emit, billing record.
- `gw-proxy` — Pingora ProxyHttp filters (early/request/upstream/response/logging).
- `gw-gateway` — binary, bootstrap, health server, StreamConsumer.
- Hard caps (ADR-024), freeze-estimate (ADR-004), governor RPM, circuit breaker, hot config reload.

### Known v1 gaps (from CLAUDE.md / README)
- Non-stream completion (rejected 400)
- `expires_at` on API keys
- `ip_allowlist`
- `daily_quota_tokens`
- `Vendor.timeout_overrides`
- `Pricing.model_markup`
- JWT iss/aud/nbf validation, mandatory HMAC
- Cross-protocol response body re-render
- Multi-event SSE buffering
- Retry executor on hot path
- DLQ for billing flush failures
- `Sticky` strategy TTL sweep

## Open Questions

- (none — clarified via question tool)

## Metis-derived Constraints (incorporated)

- **Rubric MUST be authored first**, before any parallel exploration. One task = define columns, status taxonomy, stratification rules, priority anchors, effort scale. Every downstream agent receives the rubric verbatim.
- **Stratification heuristic** (no authoritative spec yet):
  - `rust-proxy` = on request hot path (auth, codec, routing, rate-limit enforce, billing emit, freeze, breaker)
  - `go-web` = CRUD / admin / async / UI / OAuth flow / payment / subscription / management UI
  - `worker` = anything claimed by `docs/superpowers/specs/2026-05-19-task-worker-design.md` (read this BEFORE stratifying)
  - `shared` = both layers must implement (e.g., entity schema, error taxonomy)
  - Borderline rows → confidence = `Open question`; cross-reference into a dedicated "Architecture decisions needed" section.
- **Rust status taxonomy** (mandatory column values):
  - `Missing` — not present in any form
  - `Partial` — present with materially smaller surface
  - `Divergent` — present with different semantics
  - `Present` — semantically equivalent
  - `N/A` — Aether-only by design (frontend / admin UI)
  - `Deprecated in Aether` — marked for removal in Aether
- **Priority rubric** (migration-blocker frame):
  - `P0` = blocks a customer migrating off Aether onto the Rust gateway
  - `P1` = degrades parity but workaround exists
  - `P2` = nice-to-have / admin-only / observability
- **Effort scale**: `S/M/L/XL` with directional examples; agent uses gut feel.
- **Frontend handling**: ONE mention of `frontend/` as "not analyzed (UI scope)". No row-per-component enumeration. Same for announcements/email templates.
- **Forbidden behaviors**:
  - MUST NOT propose implementation approaches in gap rows.
  - MUST NOT silently decide architectural boundary calls — flag as `Open question`.
  - MUST NOT skip "both have it but different" gaps. These are the highest-signal class.
  - MUST NOT use free-form prose-only summaries — every gap row conforms to the rubric.
- **Mandatory dedicated tasks**:
  - Semantic-diff pass on the 4 high-overlap areas: auth, rate-limit, billing/usage, routing/candidate selection.
  - Schema-diff pass: `Aether/alembic/versions/` vs `crates/gw-store/migrations/`.
  - Completeness audit covers EVERY Aether top-level directory:
    - In-depth catalog: `src/services/`, `src/api/`, `src/modules/`, `aether-proxy/`, `aether-hub/`, `alembic/versions/` (each subdir/migration appears as ≥1 gap row OR is explicitly marked covered).
    - One-line mention only: `frontend/` (UI scope), `docs/`, `scripts/`, `tests/`, top-level deploy files (`deploy.sh`, `dev.sh`, `docker-compose*.yml`, `Dockerfile*`, `entrypoint.sh`, `gunicorn_conf.py`, `pyproject.toml`, `uv.lock`, `generate_keys.py`, `alembic.ini`), and tooling/version-control dirs (`.git/`, `.github/`, `.claude/`). The report has a "Deliberately not catalogued" section listing these.
- **Verification model (agent-executable, explicit)**:
  - File `docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md` exists, non-empty.
  - Required sections present (regex/grep against report): `## Executive Summary`, `## Rubric`, `## Stratification Rollup`, `## Per-Capability Narratives`, `## Full Gap Catalog`, `## Architecture Decisions Needed`, `## Deliberately Not Catalogued`.
  - Every entry of `ls Aether/src/services/`, `ls Aether/src/api/`, `ls Aether/src/modules/`, plus the names `aether-proxy` and `aether-hub`, appears at least once in the report body (case-insensitive grep).
  - Every alembic migration filename in `Aether/alembic/versions/` is referenced OR the report notes which schema features it introduced are covered.
  - Stratification Rollup totals (rust-proxy + go-web + worker + shared) equal the catalog row count (parsed from the table).
  - No empty / `TODO` / `???` cells in the Full Gap Catalog table (grep negative).
  - Every row whose Stratification Confidence is `Open question` also appears by Feature name in the `## Architecture Decisions Needed` section (cross-reference).
  - "Deliberately Not Catalogued" section enumerates every skipped Aether top-level item (frontend, docs, scripts, tests, deploy files) so reviewers see nothing was forgotten.
- **Final deliverable**: `docs/superpowers/specs/2026-05-20-aether-parity-gap-analysis.md`. Single file. Self-contained. Contains: exec summary, rubric, stratification rollup, per-area narratives, full flat catalog, architecture-review section.

## Scope Boundaries

- **INCLUDE**: Every Aether capability that touches a request OR is configured per-tenant/per-user/per-key. Catalog everything but stratify into rust-proxy vs go-web vs worker vs shared.
- **EXCLUDE**: Pure deployment/CI machinery, frontend pixel-level UI details (the report notes "Admin UI for X exists in Aether" but doesn't enumerate React components).
- **EXCLUDE**: Plan does not write the Rust code to close gaps — closure plans come AFTER this analysis is delivered.
