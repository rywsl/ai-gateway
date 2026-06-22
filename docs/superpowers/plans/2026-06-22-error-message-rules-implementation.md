# Error Message Rules Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Go-managed, frontend-configurable error-message rewrite system that Rust can hot-reload into a local tree cache and use on the error-response hot path.

**Architecture:** Go owns rule CRUD in a new standalone `errormessage` module and publishes a global snapshot through Postgres, optional Redis write-through, and `events:entity ns=error_message_rules id=global`. Rust owns runtime matching through an immutable compiled tree held behind `ArcSwap`; requests only read the local snapshot and never call Redis/Postgres on the hot path. Existing error passthrough masking remains in place, and the new rule only rewrites the client-visible message after the stable gateway code has been computed.

**Tech Stack:** Go 1.26 modules, chi, pgx/sqlc, Redis `events:entity`, Vue 3/Vite/Vitest, Rust workspace crates `gw-core`, `gw-store`, `gw-codec`, `gw-proxy`, `gw-gateway`, `arc-swap`, `sqlx`, `fred`.

## Global Constraints

- Go must create a standalone module named `errormessage`; do not put rule CRUD or service code inside `modules/provider`.
- Admin route prefix is `/api/admin/error-messages/rules`.
- Rule matching dimensions are fixed: provider id, model, endpoint id, api format, request path, client protocol, gateway code, HTTP status, then leaf checks.
- Empty match arrays mean wildcard for that dimension.
- Rust hot path must not access Redis, Postgres, async reload tasks, regex engines, script engines, remote callbacks, or dynamic expression evaluators.
- Rules must not reference other rules and must not support recursion or configurable loops.
- Text matching is bounded case-insensitive substring over at most the first 8 KiB of the upstream message.
- Matching rewrites only the returned error `message`; stable `GW_*` code, protocol type/status, and HTTP status remain unchanged.
- Bad Rust snapshots must be rejected without replacing the current snapshot.
- Existing provider `error_passthrough_rules` stays legacy and is not auto-migrated into message rewrite rules.

---

## File Structure

### Go Repository: `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go`

- Create `modules/errormessage/go.mod`: standalone Go module with local replaces for `shared/runtime`, `shared/contracts`, `shared/platform`, and `shared/rustpact`.
- Create `modules/errormessage/module.go`: runtime module descriptor, service construction, route registration.
- Create `modules/errormessage/api/middleware.go`: admin bearer middleware copied in style from existing admin modules.
- Create `modules/errormessage/api/routes.go`: chi route registration for `/api/admin/error-messages/rules`.
- Create `modules/errormessage/api/handlers.go`: HTTP handlers, request decoding, error mapping.
- Create `modules/errormessage/business/types.go`: DTOs, inputs, match context, constants.
- Create `modules/errormessage/business/validation.go`: rule validation, limits, normalized string handling.
- Create `modules/errormessage/business/matcher.go`: Go-side deterministic matcher for the `/test` endpoint.
- Create `modules/errormessage/business/service.go`: CRUD, snapshot write-through, and entity event publishing.
- Create `modules/errormessage/model/queries/error_message_rules.sql`: sqlc queries.
- Generate `modules/errormessage/model/sqlcgen/*`: sqlc output.
- Create `modules/errormessage/api/routes_test.go`: route smoke and auth checks.
- Create `modules/errormessage/business/validation_test.go`, `matcher_test.go`, `service_test.go`: unit tests.
- Add `migrations/00080_error_message_rules.sql`: Postgres table and index.
- Modify `sqlc.yaml`: add `errormessagemodel` generation stanza.
- Modify `go.work`: include `./modules/errormessage` via `go work use`.
- Modify `binaries/web/app/modules.go`: import and register `errormessage.New()`.
- Modify `shared/rustpact/stream.go`: add `NsEntityErrorMessageRules = "error_message_rules"`.
- Create `docs/business/errormessage/api.md`, `business.md`, `model.md`.
- Modify `docs/rust-contract/parts/01-go-as-producer.md` and `docs/rust-contract/parts/03-shared-schema.md`.
- Create `frontend/src/api/error-messages.ts`: typed client for CRUD and test endpoint.
- Create `frontend/src/views/admin/ErrorMessageRules.vue`: admin page.
- Create `frontend/src/api/__tests__/error-messages.spec.ts`: endpoint contract tests.
- Modify `frontend/src/router/index.ts`: add `/admin/error-messages`.
- Modify `frontend/src/layouts/MainLayout.vue`: add "错误消息" menu item in the 管理 group.
- Modify `frontend/src/features/providers/components/ProviderDetailDrawer.vue`: make the legacy error passthrough entry link to the new page with `provider_id`.

### Rust Repository: `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway`

- Create `crates/gw-core/src/error/message_rules.rs`: wire structs, validation, compiled tree, matcher.
- Modify `crates/gw-core/src/error/mod.rs`: export `message_rules`.
- Modify `crates/gw-codec/src/error_passthrough.rs`: add a render variant that accepts a match context and compiled rules.
- Modify `crates/gw-codec/tests/error_passthrough.rs`: assert message rewrite preserves code/type/status.
- Create `crates/gw-store/src/error_message_rules.rs`: Redis/PgSQL loader and `ErrorMessageRuleStore`.
- Modify `crates/gw-store/src/lib.rs`: export `error_message_rules`.
- Modify `crates/gw-store/src/stream_consumer.rs`: react to `ns=error_message_rules id=global` by spawning a reload.
- Create `crates/gw-store/tests/error_message_rules.rs`: loader/reload tests.
- Modify `crates/gw-proxy/src/ctx.rs`: add `selected_endpoint_id: Option<String>`.
- Modify `crates/gw-proxy/src/filters/upstream.rs`: populate `selected_endpoint_id` when available; first pass leaves it `None` if the current key projection has no row id.
- Modify `crates/gw-proxy/src/gateway.rs`: store `Arc<ErrorMessageRuleStore>`, build match context, call the new codec render function.
- Modify `crates/gw-proxy/tests/response_hot_path.rs`: assert response body filter does not introduce Redis/Pg calls into the hot path.
- Modify `crates/gw-gateway/src/lib.rs`: bootstrap the rule store, cold-load once, pass it to `LlmGateway`, and wire it into `StreamConsumer`.
- Modify `crates/gw-gateway/src/metrics_catalog.rs`: add new metrics.
- Modify `docs/go-contracts/parts/01-go-as-producer.md`, `docs/go-contracts/parts/03-shared-schema.md`, `docs/workflow.md`, and `docs/architecture.md`.

---

### Task 1: Go Standalone Module Scaffold

**Files:**
- Create: `modules/errormessage/go.mod`
- Create: `modules/errormessage/module.go`
- Create: `modules/errormessage/api/middleware.go`
- Create: `modules/errormessage/api/routes.go`
- Create: `modules/errormessage/api/routes_test.go`
- Modify: `go.work`
- Modify: `binaries/web/app/modules.go`

**Interfaces:**
- Consumes: `runtime.Deps`, `contracts.TokenVerifier`, `rustpact.EntityPusher`.
- Produces: `errormessage.New() *Module`, mounted admin routes under `/api/admin/error-messages/rules`.

- [ ] **Step 1: Scaffold the module**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
scripts/new-module.sh errormessage
go get -C modules/errormessage github.com/go-chi/chi/v5@v5.3.0 github.com/jackc/pgx/v5@v5.9.2 github.com/redis/go-redis/v9@v9.19.0 github.com/stretchr/testify@v1.11.1
go mod edit -C modules/errormessage -require=github.com/cnxx-tech/ailinkmax-go/shared/platform@v0.0.0
go mod edit -C modules/errormessage -require=github.com/cnxx-tech/ailinkmax-go/shared/rustpact@v0.0.0
go mod edit -C modules/errormessage -replace=github.com/cnxx-tech/ailinkmax-go/shared/platform=../../shared/platform
go mod edit -C modules/errormessage -replace=github.com/cnxx-tech/ailinkmax-go/shared/rustpact=../../shared/rustpact
go mod tidy -C modules/errormessage
```

Expected: module files exist and `go.work` includes `./modules/errormessage`.

- [ ] **Step 2: Replace generated `module.go` with the runtime module**

Use this structure:

```go
package errormessage

import (
	"io/fs"

	"github.com/go-chi/chi/v5"

	"github.com/cnxx-tech/ailinkmax-go/modules/errormessage/api"
	"github.com/cnxx-tech/ailinkmax-go/modules/errormessage/business"
	"github.com/cnxx-tech/ailinkmax-go/shared/contracts"
	"github.com/cnxx-tech/ailinkmax-go/shared/runtime"
	"github.com/cnxx-tech/ailinkmax-go/shared/rustpact"
)

type Module struct {
	svc *business.Service
}

func New() *Module { return &Module{} }

func (*Module) Name() string { return "errormessage" }

func (m *Module) Provide(d runtime.Deps, _ *contracts.Registry) {
	m.svc = business.NewService(d.DB, rustpact.NewEntityPusher(d.Redis), d.Redis)
}

func (m *Module) Routes(r chi.Router, d runtime.Deps) {
	if m.svc == nil {
		m.svc = business.NewService(d.DB, rustpact.NewEntityPusher(d.Redis), d.Redis)
	}
	verifier := contracts.MustResolve[contracts.TokenVerifier](d.Reg)
	api.Register(r, api.NewHandlers(m.svc), verifier)
}

func (*Module) Workers() []runtime.Worker { return nil }

func (*Module) Migrations() fs.FS { return nil }
```

- [ ] **Step 3: Add route smoke test first**

Create `modules/errormessage/api/routes_test.go`:

```go
package api_test

import (
	"testing"

	"github.com/go-chi/chi/v5"
	"github.com/stretchr/testify/require"

	"github.com/cnxx-tech/ailinkmax-go/modules/errormessage"
	"github.com/cnxx-tech/ailinkmax-go/shared/contracts"
	"github.com/cnxx-tech/ailinkmax-go/shared/contracts/contracttest"
	"github.com/cnxx-tech/ailinkmax-go/shared/runtime"
)

func TestRoutesSmoke(t *testing.T) {
	reg := contracts.NewRegistry()
	contracts.Register[contracts.TokenVerifier](reg, contracttest.NewTokenVerifier())
	m := errormessage.New()
	m.Provide(runtime.Deps{Reg: reg}, reg)
	require.NotPanics(t, func() {
		m.Routes(chi.NewRouter(), runtime.Deps{Reg: reg})
	})
}
```

- [ ] **Step 4: Run the smoke test and observe failure**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
go test -C modules/errormessage ./api -run TestRoutesSmoke -count=1
```

Expected: FAIL until `api.Register`, middleware, and handlers exist.

- [ ] **Step 5: Add minimal API registration**

Create `modules/errormessage/api/routes.go`:

```go
package api

import (
	"github.com/go-chi/chi/v5"

	"github.com/cnxx-tech/ailinkmax-go/shared/contracts"
)

func Register(r chi.Router, h *Handlers, verifier contracts.TokenVerifier) {
	r.With(RequireAdmin(verifier)).Route("/api/admin/error-messages/rules", func(r chi.Router) {
		r.Get("/", h.ListRules)
		r.Post("/", h.CreateRule)
		r.Get("/{id}", h.GetRule)
		r.Patch("/{id}", h.UpdateRule)
		r.Delete("/{id}", h.DeleteRule)
		r.Post("/test", h.TestRule)
	})
}
```

Create `modules/errormessage/api/middleware.go` using the same `RequireAdmin`, `claimsFrom`, `writeJSON`, and `writeError` pattern from `modules/budgetpolicy/api/middleware.go`.

Create placeholder handlers that fail closed while service code is not present:

```go
package api

import (
	"net/http"

	"github.com/cnxx-tech/ailinkmax-go/modules/errormessage/business"
)

type Handlers struct {
	svc *business.Service
}

func NewHandlers(svc *business.Service) *Handlers { return &Handlers{svc: svc} }

func (h *Handlers) ListRules(w http.ResponseWriter, r *http.Request)  { writeError(w, http.StatusServiceUnavailable, "errormessage_unavailable") }
func (h *Handlers) CreateRule(w http.ResponseWriter, r *http.Request) { writeError(w, http.StatusServiceUnavailable, "errormessage_unavailable") }
func (h *Handlers) GetRule(w http.ResponseWriter, r *http.Request)    { writeError(w, http.StatusServiceUnavailable, "errormessage_unavailable") }
func (h *Handlers) UpdateRule(w http.ResponseWriter, r *http.Request) { writeError(w, http.StatusServiceUnavailable, "errormessage_unavailable") }
func (h *Handlers) DeleteRule(w http.ResponseWriter, r *http.Request) { writeError(w, http.StatusServiceUnavailable, "errormessage_unavailable") }
func (h *Handlers) TestRule(w http.ResponseWriter, r *http.Request)   { writeError(w, http.StatusServiceUnavailable, "errormessage_unavailable") }
```

- [ ] **Step 6: Register the module in the web app**

Modify `binaries/web/app/modules.go`:

```go
import "github.com/cnxx-tech/ailinkmax-go/modules/errormessage"
```

Add `errormessage.New()` next to other admin/business modules:

```go
license.New(), gatewaycontrol.New(), errormessage.New(), system.New(), budgetpolicy.New(),
```

- [ ] **Step 7: Verify and commit**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
go test -C modules/errormessage ./... -count=1
go test ./binaries/web/app/... -count=1
```

Expected: PASS.

Commit:

```bash
git add go.work modules/errormessage binaries/web/app/modules.go
git commit -m "feat: scaffold error message module"
```

---

### Task 2: Go Schema, Validation, Matcher, and Service

**Files:**
- Create: `migrations/00080_error_message_rules.sql`
- Create: `modules/errormessage/model/queries/error_message_rules.sql`
- Generate: `modules/errormessage/model/sqlcgen/*`
- Modify: `sqlc.yaml`
- Create: `modules/errormessage/business/types.go`
- Create: `modules/errormessage/business/validation.go`
- Create: `modules/errormessage/business/matcher.go`
- Create: `modules/errormessage/business/service.go`
- Create: `modules/errormessage/business/validation_test.go`
- Create: `modules/errormessage/business/matcher_test.go`
- Create: `modules/errormessage/business/service_test.go`
- Modify: `shared/rustpact/stream.go`

**Interfaces:**
- Produces: `business.Service` methods `List`, `Get`, `Create`, `Update`, `Delete`, `Test`, `Snapshot`.
- Produces: `business.MatchRule(rules []RuleDTO, ctx MatchContext, upstreamMessage string) (*RuleDTO, []string)`.
- Produces Redis hash `entity:error_message_rules:global` with `{v, enc=json, d=<json array>}`.

- [ ] **Step 1: Add migration**

Create `migrations/00080_error_message_rules.sql`:

```sql
CREATE TABLE IF NOT EXISTS error_message_rules (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  enabled BOOLEAN NOT NULL DEFAULT TRUE,
  priority INTEGER NOT NULL DEFAULT 1000,
  match_json JSONB NOT NULL DEFAULT '{}'::jsonb,
  message TEXT NOT NULL,
  notes TEXT,
  version BIGINT NOT NULL DEFAULT 1,
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL,
  deleted_at BIGINT
);

CREATE INDEX IF NOT EXISTS idx_error_message_rules_enabled_priority
  ON error_message_rules (enabled, priority, id)
  WHERE deleted_at IS NULL;
```

- [ ] **Step 2: Add sqlc queries and config**

Create `modules/errormessage/model/queries/error_message_rules.sql` with named queries:

```sql
-- name: CreateRule :one
INSERT INTO error_message_rules (
  id, name, enabled, priority, match_json, message, notes, version, created_at, updated_at
) VALUES (
  $1, $2, $3, $4, $5, $6, $7, 1, $8, $8
) RETURNING *;

-- name: GetRule :one
SELECT * FROM error_message_rules WHERE id = $1 AND deleted_at IS NULL;

-- name: ListRules :many
SELECT * FROM error_message_rules
WHERE deleted_at IS NULL
  AND (sqlc.arg(search)::text = '' OR name ILIKE '%' || sqlc.arg(search)::text || '%' OR message ILIKE '%' || sqlc.arg(search)::text || '%')
  AND (sqlc.arg(enabled)::text = '' OR enabled = sqlc.arg(enabled)::boolean)
  AND (sqlc.arg(provider_id)::text = '' OR match_json->'provider_ids' ? sqlc.arg(provider_id)::text)
  AND (sqlc.arg(model)::text = '' OR match_json->'models' ? sqlc.arg(model)::text)
  AND (sqlc.arg(api_format)::text = '' OR match_json->'api_formats' ? sqlc.arg(api_format)::text)
  AND (sqlc.arg(gateway_code)::text = '' OR match_json->'gateway_codes' ? sqlc.arg(gateway_code)::text)
  AND (sqlc.arg(http_status)::int = 0 OR match_json->'http_statuses' ? (sqlc.arg(http_status)::text))
ORDER BY enabled DESC, priority ASC, id ASC
LIMIT $1 OFFSET $2;

-- name: CountRules :one
SELECT count(*) FROM error_message_rules
WHERE deleted_at IS NULL
  AND (sqlc.arg(search)::text = '' OR name ILIKE '%' || sqlc.arg(search)::text || '%' OR message ILIKE '%' || sqlc.arg(search)::text || '%')
  AND (sqlc.arg(enabled)::text = '' OR enabled = sqlc.arg(enabled)::boolean);

-- name: UpdateRule :one
UPDATE error_message_rules
SET name = $2,
    enabled = $3,
    priority = $4,
    match_json = $5,
    message = $6,
    notes = $7,
    version = version + 1,
    updated_at = $8
WHERE id = $1 AND deleted_at IS NULL
RETURNING *;

-- name: SoftDeleteRule :execrows
UPDATE error_message_rules
SET deleted_at = $2, version = version + 1, updated_at = $2
WHERE id = $1 AND deleted_at IS NULL;

-- name: ListEnabledRulesForSnapshot :many
SELECT * FROM error_message_rules
WHERE enabled = TRUE AND deleted_at IS NULL
ORDER BY priority ASC, id ASC;

-- name: CountEnabledRules :one
SELECT count(*) FROM error_message_rules
WHERE enabled = TRUE AND deleted_at IS NULL;
```

Add this stanza to `sqlc.yaml`:

```yaml
  - schema: migrations/
    queries: modules/errormessage/model/queries/
    engine: postgresql
    gen:
      go:
        package: errormessagemodel
        out: modules/errormessage/model/sqlcgen
        sql_package: pgx/v5
        emit_json_tags: false
        emit_db_tags: true
```

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
sqlc generate
```

- [ ] **Step 3: Write failing validation tests**

Create tests that lock the limits:

```go
func TestValidateRejectsScriptsRegexAndTooManyValues(t *testing.T) {
	in := UpsertRuleInput{
		Name: "bad",
		Enabled: true,
		Priority: 100,
		Match: RuleMatch{UpstreamMessageContains: []string{"(?i)secret"}},
		Message: "use the right endpoint",
	}
	require.ErrorIs(t, ValidateUpsert(in), ErrInvalidRule)

	values := make([]string, 51)
	for i := range values { values[i] = fmt.Sprintf("v%d", i) }
	in.Match = RuleMatch{ProviderIDs: values}
	require.ErrorIs(t, ValidateUpsert(in), ErrInvalidRule)
}

func TestValidateAllowsPlainContainsFragments(t *testing.T) {
	in := UpsertRuleInput{
		Name: "image wrong endpoint",
		Enabled: true,
		Priority: 100,
		Match: RuleMatch{
			ProviderIDs: []string{"v_openai"},
			Models: []string{"gpt-image-2-max"},
			RequestPaths: []string{"/v1/chat/completions"},
			HTTPStatuses: []int{400},
			GatewayCodes: []string{"GW_UPSTREAM_4XX"},
			UpstreamMessageContains: []string{"unsupported"},
		},
		Message: "use image generation endpoint",
	}
	require.NoError(t, ValidateUpsert(in))
}
```

Run:

```bash
go test -C modules/errormessage ./business -run 'TestValidate' -count=1
```

Expected: FAIL until validation exists.

- [ ] **Step 4: Implement DTOs and validation**

Define these types in `business/types.go`:

```go
type RuleMatch struct {
	ProviderIDs               []string `json:"provider_ids"`
	Models                    []string `json:"models"`
	EndpointIDs               []string `json:"endpoint_ids"`
	APIFormats                []string `json:"api_formats"`
	RequestPaths              []string `json:"request_paths"`
	ClientProtocols           []string `json:"client_protocols"`
	HTTPStatuses              []int    `json:"http_statuses"`
	GatewayCodes              []string `json:"gateway_codes"`
	UpstreamMessageContains   []string `json:"upstream_message_contains"`
}

type RuleDTO struct {
	ID        string    `json:"id"`
	Name      string    `json:"name"`
	Enabled   bool      `json:"enabled"`
	Priority  int       `json:"priority"`
	Match     RuleMatch `json:"match"`
	Message   string    `json:"message"`
	Notes     *string   `json:"notes,omitempty"`
	Version   int64     `json:"version"`
	CreatedAt int64     `json:"created_at"`
	UpdatedAt int64     `json:"updated_at"`
}

type MatchContext struct {
	ProviderID     string `json:"provider_id"`
	Model          string `json:"model"`
	EndpointID     string `json:"endpoint_id"`
	APIFormat      string `json:"api_format"`
	RequestPath    string `json:"request_path"`
	ClientProtocol string `json:"client_protocol"`
	HTTPStatus     int    `json:"http_status"`
	GatewayCode    string `json:"gateway_code"`
}
```

Validation constants:

```go
const (
	MaxValuesPerDimension = 50
	MaxContainsFragments  = 10
	MaxContainsBytes      = 256
	MaxMessageBytes       = 1000
	MaxNotesBytes         = 4000
	MaxEnabledRules       = 5000
	MaxExpandedBranches   = 100000
)
```

Validation rules:

```go
var ErrInvalidRule = errors.New("errormessage: invalid rule")

func ValidateUpsert(in UpsertRuleInput) error {
	// Trim name/message/notes.
	// Reject empty name or message.
	// Reject priority outside 0..1_000_000.
	// Reject HTTP statuses outside 100..599.
	// Reject any dimension with more than MaxValuesPerDimension entries.
	// Reject more than MaxContainsFragments contains fragments.
	// Reject empty contains fragments and fragments longer than MaxContainsBytes.
	// Reject regex/script markers: "(?", "(?i)", ".*", "[", "]", "=>", "function", "while", "for(", "eval", "lua", "wasm", "cel:".
	// Compute expanded branch product using max(1, len(dim)); reject above MaxExpandedBranches.
	return nil
}
```

The regex/script marker check is a usability guard; the safety boundary is that the service stores plain strings and neither Go nor Rust compiles them as regex or code.

- [ ] **Step 5: Write matcher tests**

Create `business/matcher_test.go`:

```go
func TestMatchRuleExactAndWildcardTree(t *testing.T) {
	rules := []RuleDTO{
		{ID: "wild", Enabled: true, Priority: 200, Match: RuleMatch{Models: []string{"gpt-image-2-max"}}, Message: "wild"},
		{ID: "exact", Enabled: true, Priority: 100, Match: RuleMatch{
			ProviderIDs: []string{"v_openai"},
			Models: []string{"gpt-image-2-max"},
			RequestPaths: []string{"/v1/chat/completions"},
			GatewayCodes: []string{"GW_UPSTREAM_4XX"},
			HTTPStatuses: []int{400},
			UpstreamMessageContains: []string{"unsupported"},
		}, Message: "exact"},
	}
	got, path := MatchRule(rules, MatchContext{
		ProviderID: "v_openai", Model: "gpt-image-2-max",
		RequestPath: "/v1/chat/completions", GatewayCode: "GW_UPSTREAM_4XX", HTTPStatus: 400,
	}, "image unsupported on chat")
	require.NotNil(t, got)
	require.Equal(t, "exact", got.ID)
	require.Contains(t, strings.Join(path, "/"), "provider_id=v_openai")
}

func TestMatchRuleMissShortCircuits(t *testing.T) {
	rules := []RuleDTO{{ID: "r1", Enabled: true, Priority: 1, Match: RuleMatch{ProviderIDs: []string{"v_other"}}, Message: "no"}}
	got, _ := MatchRule(rules, MatchContext{ProviderID: "v_openai"}, "anything")
	require.Nil(t, got)
}
```

- [ ] **Step 6: Implement matcher**

Implement `MatchRule` as deterministic sorted filtering:

```go
func MatchRule(rules []RuleDTO, ctx MatchContext, upstreamMessage string) (*RuleDTO, []string) {
	sort.SliceStable(rules, func(i, j int) bool {
		if rules[i].Priority != rules[j].Priority { return rules[i].Priority < rules[j].Priority }
		return rules[i].ID < rules[j].ID
	})
	hay := lowerPrefix(upstreamMessage, 8*1024)
	for i := range rules {
		r := rules[i]
		if !r.Enabled { continue }
		if !matchStringDim(r.Match.ProviderIDs, ctx.ProviderID) { continue }
		if !matchStringDim(r.Match.Models, ctx.Model) { continue }
		if !matchStringDim(r.Match.EndpointIDs, ctx.EndpointID) { continue }
		if !matchStringDim(r.Match.APIFormats, ctx.APIFormat) { continue }
		if !matchStringDim(r.Match.RequestPaths, ctx.RequestPath) { continue }
		if !matchStringDim(r.Match.ClientProtocols, ctx.ClientProtocol) { continue }
		if !matchStringDim(r.Match.GatewayCodes, ctx.GatewayCode) { continue }
		if !matchIntDim(r.Match.HTTPStatuses, ctx.HTTPStatus) { continue }
		if !containsAll(hay, r.Match.UpstreamMessageContains) { continue }
		return &r, explainPath(r, ctx)
	}
	return nil, nil
}
```

This Go matcher is for admin preview. Rust remains runtime authority.

- [ ] **Step 7: Implement service with snapshot publishing**

`business.Service` must:

```go
type Service struct {
	q      *errormessagemodel.Queries
	pusher *rustpact.EntityPusher
	redis  redis.UniversalClient
}

func NewService(db *pgxpool.Pool, pusher *rustpact.EntityPusher, redis redis.UniversalClient) *Service
func (s *Service) List(ctx context.Context, filter RuleListFilter, limit, offset int32) ([]RuleDTO, int64, error)
func (s *Service) Get(ctx context.Context, id string) (RuleDTO, error)
func (s *Service) Create(ctx context.Context, in UpsertRuleInput) (RuleDTO, error)
func (s *Service) Update(ctx context.Context, id string, in UpsertRuleInput) (RuleDTO, error)
func (s *Service) Delete(ctx context.Context, id string) error
func (s *Service) Test(ctx context.Context, in TestRuleInput) (TestRuleOutput, error)
func (s *Service) Snapshot(ctx context.Context) ([]RuleDTO, int64, error)
```

After create/update/delete:

```go
func (s *Service) publishSnapshot(ctx context.Context) {
	rules, version, err := s.Snapshot(ctx)
	if err == nil && s.redis != nil {
		payload, _ := json.Marshal(rules)
		_ = s.redis.HSet(ctx, "entity:error_message_rules:global", map[string]any{
			"v": version,
			"enc": "json",
			"d": payload,
		}).Err()
		_ = s.redis.Expire(ctx, "entity:error_message_rules:global", 300*time.Second).Err()
	}
	if s.pusher != nil {
		s.pusher.Push(ctx, rustpact.NsEntityErrorMessageRules, "global", version, "upsert")
	}
}
```

Write-through failure is logged and does not roll back the database write.

- [ ] **Step 8: Add rustpact namespace**

Modify `shared/rustpact/stream.go`:

```go
const (
	NsEntityErrorMessageRules = "error_message_rules"
)
```

Place it in the events:entity namespace section with a comment that Rust accepts it once this gateway implementation is deployed.

- [ ] **Step 9: Run tests and commit**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
sqlc generate
go test -C modules/errormessage ./... -count=1
go test ./shared/rustpact/... -count=1
```

Expected: PASS.

Commit:

```bash
git add migrations/00080_error_message_rules.sql sqlc.yaml shared/rustpact/stream.go modules/errormessage
git commit -m "feat: add error message rule service"
```

---

### Task 3: Go Admin API Handlers and Contract Tests

**Files:**
- Modify: `modules/errormessage/api/handlers.go`
- Modify: `modules/errormessage/api/routes_test.go`
- Create: `docs/business/errormessage/api.md`

**Interfaces:**
- Produces admin JSON API:
  - `GET /api/admin/error-messages/rules`
  - `POST /api/admin/error-messages/rules`
  - `GET /api/admin/error-messages/rules/{id}`
  - `PATCH /api/admin/error-messages/rules/{id}`
  - `DELETE /api/admin/error-messages/rules/{id}`
  - `POST /api/admin/error-messages/rules/test`

- [ ] **Step 1: Write handler tests**

Use a fake service interface in `api/routes_test.go` and assert route shape:

```go
func TestRulesRequireAdmin(t *testing.T) {
	r := chi.NewRouter()
	api.Register(r, api.NewHandlers(fakeService{}), contracttest.NewTokenVerifier())
	req := httptest.NewRequest(http.MethodGet, "/api/admin/error-messages/rules", nil)
	rr := httptest.NewRecorder()
	r.ServeHTTP(rr, req)
	require.Equal(t, http.StatusUnauthorized, rr.Code)
}
```

Add a create request test:

```go
func TestCreateRuleReturnsCreatedRule(t *testing.T) {
	body := `{"name":"image wrong endpoint","enabled":true,"priority":100,"match":{"models":["gpt-image-2-max"]},"message":"use image endpoint"}`
	req := authedAdminRequest(http.MethodPost, "/api/admin/error-messages/rules", body)
	rr := httptest.NewRecorder()
	routerWithFakeService().ServeHTTP(rr, req)
	require.Equal(t, http.StatusCreated, rr.Code)
	require.Contains(t, rr.Body.String(), `"message":"use image endpoint"`)
}
```

- [ ] **Step 2: Implement concrete handlers**

Map errors:

```go
ErrInvalidRule -> 400 errormessage_invalid_rule
ErrNotFound -> 404 errormessage_rule_not_found
nil db/service -> 503 errormessage_unavailable
default -> 500 internal
```

`ListRules` reads `page`, `page_size`, `search`, `enabled`, `provider_id`, `model`, `api_format`, `gateway_code`, `http_status`.

`TestRule` returns:

```json
{
  "matched": true,
  "rule_id": "emr_...",
  "message": "该模型是生图模型，请使用图片生成接口提交任务，不要通过文本对话接口调用。",
  "path": ["provider_id=v_openai", "model=gpt-image-2-max"]
}
```

- [ ] **Step 3: Document API**

`docs/business/errormessage/api.md` must include request/response examples and the fact that `match.endpoint_ids` is supported even when some Rust paths initially provide no endpoint id.

- [ ] **Step 4: Verify and commit**

Run:

```bash
go test -C modules/errormessage ./api ./business -count=1
```

Commit:

```bash
git add modules/errormessage docs/business/errormessage/api.md
git commit -m "feat: expose error message rule admin api"
```

---

### Task 4: Frontend Admin Page

**Files:**
- Create: `frontend/src/api/error-messages.ts`
- Create: `frontend/src/api/__tests__/error-messages.spec.ts`
- Create: `frontend/src/views/admin/ErrorMessageRules.vue`
- Modify: `frontend/src/router/index.ts`
- Modify: `frontend/src/layouts/MainLayout.vue`
- Modify: `frontend/src/features/providers/components/ProviderDetailDrawer.vue`

**Interfaces:**
- Consumes Go admin API from Task 3.
- Produces `/admin/error-messages` page with list, filters, edit drawer, and test panel.

- [ ] **Step 1: Write API client tests**

Create `frontend/src/api/__tests__/error-messages.spec.ts` using the existing axios mock style:

```ts
import { describe, expect, it, vi } from 'vitest'
import client from '../client'
import { listErrorMessageRules, testErrorMessageRule } from '../error-messages'

vi.mock('../client', () => ({ default: { get: vi.fn(), post: vi.fn() } }))

it('lists error message rules with filters', async () => {
  vi.mocked(client.get).mockResolvedValueOnce({ data: { total: 1, page: 1, page_size: 20, items: [] } })
  await listErrorMessageRules({ provider_id: 'v_openai', model: 'gpt-image-2-max' })
  expect(client.get).toHaveBeenCalledWith('/api/admin/error-messages/rules', {
    params: { provider_id: 'v_openai', model: 'gpt-image-2-max' },
  })
})

it('tests rule matches', async () => {
  vi.mocked(client.post).mockResolvedValueOnce({ data: { matched: true, rule_id: 'emr_1', message: 'use image endpoint', path: [] } })
  const result = await testErrorMessageRule({ context: { model: 'gpt-image-2-max' }, upstream_message: 'unsupported' })
  expect(result.matched).toBe(true)
})
```

- [ ] **Step 2: Implement API client**

`frontend/src/api/error-messages.ts` exports:

```ts
export interface ErrorMessageRuleMatch {
  provider_ids: string[]
  models: string[]
  endpoint_ids: string[]
  api_formats: string[]
  request_paths: string[]
  client_protocols: string[]
  http_statuses: number[]
  gateway_codes: string[]
  upstream_message_contains: string[]
}

export interface ErrorMessageRule {
  id: string
  name: string
  enabled: boolean
  priority: number
  match: ErrorMessageRuleMatch
  message: string
  notes?: string
  version: number
  created_at: number
  updated_at: number
}
```

Add `listErrorMessageRules`, `createErrorMessageRule`, `updateErrorMessageRule`, `deleteErrorMessageRule`, and `testErrorMessageRule`.

- [ ] **Step 3: Add route and nav**

`frontend/src/router/index.ts`:

```ts
{
  path: 'error-messages',
  name: 'ErrorMessageRules',
  component: () => importWithRetry(() => import('@/views/admin/ErrorMessageRules.vue'))
}
```

`frontend/src/layouts/MainLayout.vue`:

```ts
{ name: '错误消息', href: '/admin/error-messages', icon: MessageSquareWarning },
```

Import `MessageSquareWarning` from `lucide-vue-next`.

- [ ] **Step 4: Build the page**

`ErrorMessageRules.vue` layout:

```vue
<template>
  <PageContainer>
    <PageHeader title="错误消息" />
    <section class="space-y-3">
      <!-- filters -->
      <!-- table -->
      <!-- edit drawer/dialog -->
      <!-- test panel inside the editor -->
    </section>
  </PageContainer>
</template>
```

Required controls:
- Search input.
- Enabled select.
- Provider/model/api format/gateway code/status filters.
- Table columns: name, enabled, priority, match summary, message, updated_at, actions.
- Edit form fields: name, enabled switch, priority number input, message textarea, notes textarea.
- Match inputs as multi-value token fields using plain strings and number input for statuses.
- Contains field label is `包含片段`; do not use the word `正则` in UI copy.
- Test panel posts `context` plus `upstream_message` and displays matched rule/message/path.

- [ ] **Step 5: Link legacy provider drawer to the new page**

In `ProviderDetailDrawer.vue`, keep the existing legacy dialog available only for passthrough/masking. Add a button or replace the old message-oriented entry with:

```ts
router.push({ path: '/admin/error-messages', query: { provider_id: props.provider?.id } })
```

Visible copy: `配置错误消息`.

- [ ] **Step 6: Verify and commit**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/frontend
npm run test:run -- src/api/__tests__/error-messages.spec.ts
npm run type-check
```

Commit:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add frontend/src/api/error-messages.ts frontend/src/api/__tests__/error-messages.spec.ts frontend/src/views/admin/ErrorMessageRules.vue frontend/src/router/index.ts frontend/src/layouts/MainLayout.vue frontend/src/features/providers/components/ProviderDetailDrawer.vue
git commit -m "feat: add error message rules admin page"
```

---

### Task 5: Rust Core Rule Compiler and Matcher

**Files:**
- Create: `crates/gw-core/src/error/message_rules.rs`
- Modify: `crates/gw-core/src/error/mod.rs`

**Interfaces:**
- Produces `ErrorMessageRule`, `ErrorMessageMatcher`, `ErrorMessageMatchCtx`, `CompiledErrorMessageRules`.
- Produces `CompiledErrorMessageRules::compile(Vec<ErrorMessageRule>) -> Result<Self, CompileError>`.
- Produces `CompiledErrorMessageRules::match_message(&self, ctx: &ErrorMessageMatchCtx, upstream_message: &str) -> Option<&str>`.

- [ ] **Step 1: Write failing unit tests**

In `message_rules.rs` tests:

```rust
#[test]
fn exact_branch_wins_by_priority_and_id() {
    let rules = vec![
        rule("b", 200, matcher().model("gpt-image-2-max"), "wild"),
        rule("a", 100, matcher().provider("v_openai").model("gpt-image-2-max").status(400).contains("unsupported"), "exact"),
    ];
    let compiled = CompiledErrorMessageRules::compile(rules).unwrap();
    let ctx = ErrorMessageMatchCtx {
        provider_id: Some("v_openai"),
        model: Some("gpt-image-2-max"),
        http_status: Some(400),
        ..Default::default()
    };
    assert_eq!(compiled.match_message(&ctx, "image unsupported").unwrap(), "exact");
}

#[test]
fn miss_short_circuits_without_leaf_scan() {
    let compiled = CompiledErrorMessageRules::compile(vec![
        rule("r1", 1, matcher().provider("v_other"), "no"),
    ]).unwrap();
    let ctx = ErrorMessageMatchCtx { provider_id: Some("v_openai"), ..Default::default() };
    assert!(compiled.match_message(&ctx, "anything").is_none());
}
```

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo test -p gw-core error::message_rules --lib
```

Expected: FAIL until the module exists.

- [ ] **Step 2: Implement wire structs**

Use owned strings in rules and borrowed strings in match context:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ErrorMessageRule {
    pub id: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub priority: u32,
    #[serde(rename = "match")]
    pub matcher: ErrorMessageMatcher,
    pub message: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ErrorMessageMatcher {
    #[serde(default)]
    pub provider_ids: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub endpoint_ids: Vec<String>,
    #[serde(default)]
    pub api_formats: Vec<String>,
    #[serde(default)]
    pub request_paths: Vec<String>,
    #[serde(default)]
    pub client_protocols: Vec<String>,
    #[serde(default)]
    pub http_statuses: Vec<u16>,
    #[serde(default)]
    pub gateway_codes: Vec<String>,
    #[serde(default)]
    pub upstream_message_contains: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ErrorMessageMatchCtx<'a> {
    pub provider_id: Option<&'a str>,
    pub model: Option<&'a str>,
    pub endpoint_id: Option<&'a str>,
    pub api_format: Option<&'a str>,
    pub request_path: Option<&'a str>,
    pub client_protocol: Option<&'a str>,
    pub http_status: Option<u16>,
    pub gateway_code: Option<&'a str>,
}
```

- [ ] **Step 3: Implement fixed-depth compiled tree**

Core shape:

```rust
#[derive(Default)]
struct RuleNode {
    exact: std::collections::BTreeMap<String, RuleNode>,
    wildcard: Option<Box<RuleNode>>,
    leaves: Vec<CompiledRule>,
}

pub struct CompiledErrorMessageRules {
    root: RuleNode,
    rule_count: usize,
    node_count: usize,
}
```

Compile inserts dimensions in this order:

```rust
provider_id, model, endpoint_id, api_format, request_path, client_protocol, gateway_code, http_status
```

For each dimension, an empty rule vector inserts the wildcard branch. A non-empty vector inserts exact branches for every value. Reject any snapshot if:
- enabled rules exceed 5000;
- any dimension has more than 50 values;
- contains fragments exceed 10;
- any contains fragment is empty or longer than 256 bytes;
- expanded branch count exceeds 100000.

Search visits at most exact branch plus wildcard branch at each fixed depth. Use a small stack of node references per depth; there is no user-configurable loop or rule recursion.

- [ ] **Step 4: Export module and verify**

Modify `crates/gw-core/src/error/mod.rs`:

```rust
pub mod message_rules;
```

Run:

```bash
cargo test -p gw-core message_rules --lib
```

Expected: PASS.

Commit:

```bash
git add crates/gw-core/src/error/message_rules.rs crates/gw-core/src/error/mod.rs
git commit -m "feat: compile error message rule tree"
```

---

### Task 6: Rust Store Loader and Stream Reload

**Files:**
- Create: `crates/gw-store/src/error_message_rules.rs`
- Modify: `crates/gw-store/src/lib.rs`
- Modify: `crates/gw-store/src/stream_consumer.rs`
- Create: `crates/gw-store/tests/error_message_rules.rs`

**Interfaces:**
- Produces `ErrorMessageRuleStore::new(loader: Option<LoaderCtx>) -> Self`.
- Produces `async fn reload(&self) -> anyhow::Result<u64>`.
- Produces `fn snapshot(&self) -> Arc<CompiledErrorMessageRules>`.

- [ ] **Step 1: Write loader tests**

Create `crates/gw-store/tests/error_message_rules.rs`:

```rust
#[tokio::test]
async fn bad_snapshot_keeps_old_compiled_rules() {
    let store = ErrorMessageRuleStore::empty_with_rules(vec![valid_rule("old")]).unwrap();
    store.replace_for_test(vec![invalid_rule_with_too_many_values()]).await;
    assert_eq!(store.snapshot().rule_count(), 1);
}
```

Add a stream consumer unit test using a test hook:

```rust
#[tokio::test]
async fn stream_event_triggers_error_message_reload() {
    let rules = Arc::new(TestReloadCounter::default());
    apply_error_message_rules_event(Some(rules.clone()), "error_message_rules", "global").await;
    assert_eq!(rules.count(), 1);
}
```

- [ ] **Step 2: Implement loader**

`error_message_rules.rs`:

```rust
pub struct ErrorMessageRuleStore {
    loader: Option<Arc<LoaderCtx>>,
    current: arc_swap::ArcSwap<CompiledErrorMessageRules>,
}

impl ErrorMessageRuleStore {
    pub fn new(loader: Option<LoaderCtx>) -> Self
    pub fn empty() -> Self
    pub fn snapshot(&self) -> Arc<CompiledErrorMessageRules>
    pub async fn reload(&self) -> anyhow::Result<u64>
}
```

Reload sequence:
1. Try Redis `HGETALL entity:error_message_rules:global`.
2. If present, parse `{v, enc, d}` where `d` is JSON array of `ErrorMessageRule`.
3. If absent, query PgSQL:

```sql
SELECT id, enabled, priority, match_json, message, version
FROM error_message_rules
WHERE enabled = TRUE AND deleted_at IS NULL
ORDER BY priority ASC, id ASC
```

4. Compile rules using `CompiledErrorMessageRules::compile`.
5. On success, `self.current.store(Arc::new(compiled))`.
6. On compile/load error, increment `gw_error_message_rules_reload_total{result="error"}` and return error without storing.

- [ ] **Step 3: Wire stream consumer**

Modify `StreamConsumer`:

```rust
pub struct StreamConsumer {
    pub redis: RedisCtx,
    pub store: Arc<EntityStore>,
    pub error_message_rules: Option<Arc<crate::error_message_rules::ErrorMessageRuleStore>>,
    pub instance_id: String,
}
```

In entity event handling:

```rust
if ns == "error_message_rules" && id == "global" {
    if let Some(rules) = self.error_message_rules.clone() {
        tokio::spawn(async move {
            if let Err(err) = rules.reload().await {
                tracing::warn!(target: "gw_store::error_message_rules", error = %err, "reload failed; keeping old snapshot");
            }
        });
    }
    return;
}
```

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p gw-store error_message_rules stream_consumer --tests --lib
```

Commit:

```bash
git add crates/gw-store/src/error_message_rules.rs crates/gw-store/src/lib.rs crates/gw-store/src/stream_consumer.rs crates/gw-store/tests/error_message_rules.rs
git commit -m "feat: load error message rules into local cache"
```

---

### Task 7: Rust Codec and Proxy Hot Path Integration

**Files:**
- Modify: `crates/gw-codec/src/error_passthrough.rs`
- Modify: `crates/gw-codec/tests/error_passthrough.rs`
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/tests/response_hot_path.rs`

**Interfaces:**
- Consumes `Arc<ErrorMessageRuleStore>` from gateway bootstrap.
- Produces message rewrite in non-stream and SSE passthrough errors.

- [ ] **Step 1: Write codec tests**

Add to `crates/gw-codec/tests/error_passthrough.rs`:

```rust
#[test]
fn message_rule_rewrites_message_but_keeps_gateway_code() {
    let body = body(400, json!({"error":{"message":"image unsupported on chat"}}));
    let compiled = CompiledErrorMessageRules::compile(vec![rule_for_image_wrong_endpoint()]).unwrap();
    let ctx = ErrorMessageMatchCtx {
        provider_id: Some("v_openai"),
        model: Some("gpt-image-2-max"),
        request_path: Some("/v1/chat/completions"),
        gateway_code: Some("GW_UPSTREAM_4XX"),
        http_status: Some(400),
        ..Default::default()
    };
    let out = render_error_to_client_with_message_rules(
        ProtocolKind::OpenAi, &body, ErrorClass::UpstreamTransient, &[], Some(&ctx), Some(&compiled)
    );
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["error"]["code"], "GW_UPSTREAM_4XX");
    assert_eq!(v["error"]["message"], "该模型是生图模型，请使用图片生成接口提交任务，不要通过文本对话接口调用。");
}
```

- [ ] **Step 2: Add codec render variant**

Keep the old function as a wrapper:

```rust
pub fn render_error_to_client(...) -> Bytes {
    render_error_to_client_with_message_rules(client, body, fallback, rules, None, None)
}
```

New variant:

```rust
pub fn render_error_to_client_with_message_rules(
    client: ProtocolKind,
    body: &PassthroughBody,
    fallback: ErrorClass,
    rules: &[PassthroughRule],
    match_ctx: Option<&ErrorMessageMatchCtx<'_>>,
    message_rules: Option<&CompiledErrorMessageRules>,
) -> Bytes
```

Inside `json_for_protocol`, compute:
1. raw upstream message;
2. legacy passthrough allow/deny base message;
3. gateway code;
4. if match ctx and compiled rules exist, run match against the raw upstream message and gateway code;
5. use matched rule message or base message.

- [ ] **Step 3: Add request context fields**

In `ReqCtx`:

```rust
pub selected_endpoint_id: Option<String>,
```

Initialize to `None` in `Default`.

In `filters/upstream.rs`, keep the first implementation conservative:

```rust
ctx.selected_endpoint_id = None;
```

If a later projection adds endpoint row id to `VendorKey`, this assignment becomes `Some(picked.endpoint_id.clone())`. This plan keeps the endpoint dimension supported in rule schema and matcher while not inventing a false endpoint id.

- [ ] **Step 4: Integrate in gateway response body filter**

Add field to `LlmGateway`:

```rust
pub(crate) error_message_rules: Arc<gw_store::error_message_rules::ErrorMessageRuleStore>,
```

Build context near the current `render_error_to_client` call:

```rust
let provider_id = ctx.picked_vendor_key.as_ref().map(|k| k.vendor_id.as_ref());
let model = ctx.model.as_ref().map(|m| m.as_ref());
let endpoint_id = ctx.selected_endpoint_id.as_deref();
let api_format = ctx.upstream_shape.map(|s| s.format_id());
let client_protocol = ctx.client_shape.map(|s| s.format_id()).or_else(|| ctx.client_codec.as_ref().map(|c| c.kind().as_str()));
let request_path = Some(session.req_header().uri.path());
let gateway_code = derived_gateway_code.as_str();
let match_ctx = ErrorMessageMatchCtx { provider_id, model, endpoint_id, api_format, request_path, client_protocol, http_status: ctx.upstream_status, gateway_code: Some(gateway_code) };
let snapshot = self.error_message_rules.snapshot();
```

Then call `render_error_to_client_with_message_rules`.

- [ ] **Step 5: Add hot path guard test**

Update `crates/gw-proxy/tests/response_hot_path.rs` to assert the response body filter source contains `error_message_rules.snapshot()` and does not contain `reload().await`, `sqlx::`, or `hgetall`.

- [ ] **Step 6: Verify and commit**

Run:

```bash
cargo test -p gw-codec --test error_passthrough
cargo test -p gw-proxy response_hot_path --test response_hot_path
```

Commit:

```bash
git add crates/gw-codec/src/error_passthrough.rs crates/gw-codec/tests/error_passthrough.rs crates/gw-proxy/src/ctx.rs crates/gw-proxy/src/filters/upstream.rs crates/gw-proxy/src/gateway.rs crates/gw-proxy/tests/response_hot_path.rs
git commit -m "feat: rewrite passthrough error messages from local rules"
```

---

### Task 8: Rust Gateway Bootstrap, Metrics, and Docs

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
- Modify: `docs/go-contracts/parts/01-go-as-producer.md`
- Modify: `docs/go-contracts/parts/03-shared-schema.md`
- Modify: `docs/workflow.md`
- Modify: `docs/architecture.md`
- Modify Go docs: `docs/rust-contract/parts/01-go-as-producer.md`, `docs/rust-contract/parts/03-shared-schema.md`, `docs/business/errormessage/business.md`, `docs/business/errormessage/model.md`

**Interfaces:**
- Produces boot-time cold load of rules and stream-triggered hot reload.
- Produces metrics catalog entries.

- [ ] **Step 1: Bootstrap the store**

In `crates/gw-gateway/src/lib.rs`, after `LoaderCtx` and before `StreamConsumer` construction:

```rust
let error_message_rules = Arc::new(gw_store::error_message_rules::ErrorMessageRuleStore::new(Some(loader_ctx.clone())));
if let Err(err) = error_message_rules.reload().await {
    tracing::warn!(target: "gw_gateway::bootstrap", error = %err, "error message rules cold load failed; starting with empty snapshot");
}
```

Pass `error_message_rules.clone()` to `LlmGateway::new` and `Some(error_message_rules.clone())` into `StreamConsumer`.

- [ ] **Step 2: Add metrics catalog entries**

Add:

```rust
MetricSpec { name: "gw_error_message_rules_snapshot_version", kind: Gauge, help: "current loaded error message rules snapshot version" }
MetricSpec { name: "gw_error_message_rules_reload_total", kind: Counter, help: "error message rules reload attempts by result" }
MetricSpec { name: "gw_error_message_rule_match_total", kind: Counter, help: "error message rule matches by rule id" }
MetricSpec { name: "gw_error_message_rule_no_match_total", kind: Counter, help: "error message rule misses" }
MetricSpec { name: "gw_error_message_rule_compile_error_total", kind: Counter, help: "error message rule snapshot compile failures" }
```

- [ ] **Step 3: Update docs**

Document:
- Go owns `error_message_rules`.
- Redis write-through key: `entity:error_message_rules:global`.
- Stream event: `events:entity ns=error_message_rules id=global`.
- Runtime behavior: Rust keeps old snapshot on bad reload.
- Endpoint id dimension exists but only matches when the Rust context has `selected_endpoint_id`.

- [ ] **Step 4: Verify and commit**

Run in Rust repo:

```bash
cargo test -p gw-gateway metrics_catalog
cargo test -p gw-store -p gw-core -p gw-codec -p gw-proxy error_message
```

Run in Go repo:

```bash
go test -C modules/errormessage ./... -count=1
npm --prefix frontend run type-check
```

Commit Rust:

```bash
git add crates/gw-gateway/src/lib.rs crates/gw-gateway/src/metrics_catalog.rs docs/go-contracts docs/workflow.md docs/architecture.md
git commit -m "feat: bootstrap error message rule reloads"
```

Commit Go:

```bash
git add docs/business/errormessage docs/rust-contract
git commit -m "docs: document error message rule contract"
```

---

## Final Verification

- [ ] Run Go module tests:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
go test -C modules/errormessage ./... -count=1
go test ./binaries/web/app/... ./shared/rustpact/... -count=1
```

- [ ] Run frontend checks:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/frontend
npm run test:run -- src/api/__tests__/error-messages.spec.ts
npm run type-check
```

- [ ] Run Rust focused tests:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo test -p gw-core message_rules --lib
cargo test -p gw-store error_message_rules stream_consumer --tests --lib
cargo test -p gw-codec --test error_passthrough
cargo test -p gw-proxy response_hot_path --test response_hot_path
cargo test -p gw-gateway metrics_catalog
```

- [ ] Manual smoke:

1. Start Go admin and Rust gateway locally with Postgres and Redis.
2. Create a rule for model `gpt-image-2-max`, request path `/v1/chat/completions`, status `400`, gateway code `GW_UPSTREAM_4XX`, contains fragment `unsupported`.
3. Confirm Go writes `entity:error_message_rules:global` and pushes `events:entity ns=error_message_rules id=global`.
4. Trigger an upstream 4xx through chat endpoint.
5. Confirm client receives the same protocol error envelope with unchanged `code` / `gateway_code` and rewritten `message`.
6. Disable the rule and confirm the gateway returns to base passthrough/masked message without restart.

