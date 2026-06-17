# WS-10 Operational Hardening — TDD Sub-Plan

> **Status**: TDD sub-plan (executable). Sibling of v1 §4 WS-10 + v2 §4 WS-10 REV-FINAL-R15-obs / REV-FINAL-LICENSE-COMM.
> **Master plan**: `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-10, §5; v2 §0 重要 1+2, §4 WS-10.
> **Mirror**: `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-10-ops-hardening.md` (byte-identical except this header).
> **Audience**: executor with no assumed Rust/Go expertise. Every step is a recipe — read the cited line, copy the literal code, run the literal command, expect the literal output.

---

## 0. Read this before starting any task

1. **Sonnet outputs are HYPOTHESES, not conclusions.** Before editing any cited file, open it with Read and confirm the line is still where this plan says it is. If the source disagrees, the **source wins**; record the deviation in a one-line `WS10-DEVIATION-N:` note inside the PR description and adjust.
2. **Verification already performed by plan author (2026-06-17)**:
   - **S1 confirmed**: `AILinkMax-Go/shared/platform/store/pg.go:21-45` — `NewPG` returns `pgxpool.NewWithConfig` with **no `Ping`**. `redis.go:6-12` `NewRedis` returns a client with no `Ping`. `clickhouse.go:44-46` `NewClickHouse` calls `clickhouse.Open` (lazy) with no `Ping`. `shared/runtime/host.go:45-46` `/livez` and `/readyz` are unconditional `WriteHeader(200)`.
   - **S7 confirmed**: `AILinkMax-Go/shared/runtime/httplog.go:61-62` writes `r.URL.Path` and `r.RemoteAddr` to a request-scoped logger that is then bound to ctx; line 102 also writes `r.UserAgent()`. Zero redaction helpers exist in the package.
   - **R15 confirmed**: `ailinkmax-gateway/crates/gw-ledger/src/inflight_reaper.rs:119-124` emits `metrics::counter!("gw_billing_emit_lag_t0_total", "request_id" => request_id.to_string(), "attempt_index" => attempt.to_string())`. `request_id` is per-request → unbounded label cardinality.
   - **R15 false positive**: `crates/gw-ledger/src/detailed_archive.rs:222` `("request_id", request_id.as_ref().to_string())` is a **Redis stream XADD field**, not a Prometheus label. Do **not** touch this file under WS-10 R15 work.
3. **Every task uses the 8-section format** below. Skipping a section blocks merge.
4. **Branch / worktree**: per master plan §6.2, WS-10 is owned by Track G (SRE). Create one branch per task (`ws10-T<id>`) and rebase before each merge.

---

## 1. Task format (8 sections, mandatory)

Each task **must** have:

1. **WHAT IT IS** — one-sentence definition + concrete file path(s).
2. **WHAT IT IS NOT** — out-of-scope items + tempting wrong shapes.
3. **WHY THIS EXISTS** — root cause, citing original-audit ID + sonnet finding ID.
4. **DO** — ordered list of literal actions.
5. **DON'T** — anti-patterns that regress.
6. **HOW (concrete)** — literal code, literal commands, literal file:line targets.
7. **VERIFY** — literal test command + literal expected output marker; log to `logs/WS10-T<id>-verify.txt`.
8. **DEPENDENCIES / OWNER** — what blocks this task, what this task blocks, who owns it.

> If a task tempts you to write "appropriate" or "as needed" — STOP. Replace with a literal value or split the task.

---

## 2. Task inventory (overview)

| ID | Title | Covers | Est |
|---|---|---|---|
| T01 | Add startup-Ping helpers in `shared/platform/store` | S1 (Go) | 0.3d |
| T02 | Wire startup Ping into all 5 Go binaries; fail-fast on error | S1 (Go) | 0.4d |
| T03 | Real `/readyz` aggregator (PG/Redis/CH + SupervisorRegistry) | S1 (Go) + REV-FINAL-V11 hook | 0.5d |
| T04 | `httplog` redaction helpers (path + UA + headers) | S7 (Go) | 0.5d |
| T05 | Wire redaction into `Observe` middleware + plog seed | S7 (Go) | 0.3d |
| T06 | Sensitive-pattern regex catalog + CI lint | S7 (Go) | 0.3d |
| T07 | Kill `request_id` Prom label in Rust (R15 inflight_reaper) | R15 | 0.3d |
| T08 | CI lint: forbid high-cardinality metric labels | R15 + REV-FINAL-V7 | 0.4d |
| T09 | Grafana dashboard migration (delete `request_id` panels, replace with exemplars / attempt_index) | REV-FINAL-R15-obs | 0.5d |
| T10 | Alert-rule migration (rewrite `request_id`-dependent queries) | REV-FINAL-R15-obs | 0.3d |
| T11 | Prometheus dashboard: WS-1 outbox invariants | WS-1 obs | 0.5d |
| T12 | Prometheus dashboard: WS-2 worker/drainer invariants | WS-2 obs | 0.5d |
| T13 | Prometheus dashboard: WS-3 bounded-memory invariants | WS-3 obs | 0.5d |
| T14 | Prometheus dashboard: WS-4 supervisor invariants | WS-4 obs | 0.3d |
| T15 | Prometheus dashboard: WS-5 fail-closed invariants | WS-5 obs | 0.3d |
| T16 | Prometheus dashboard: WS-6/7/8 invariants | WS-6/7/8 obs | 0.4d |
| T17 | Alert rule: `*_panic_recovered_total > 0` (5m) | WS-4 / WS-10 | 0.2d |
| T18 | Alert rule: `idempotency_keys_reserved_age_seconds` (5m) | WS-1 / WS-10 | 0.3d |
| T19 | Alert rule: `gw_routing_*_size` high-water (10m) | WS-3 / WS-10 | 0.2d |
| T20 | Alert rule: `gw_tunnel_active_streams` monotonic growth (15m) | WS-2 / WS-10 | 0.2d |
| T21 | Runbook: per P0 invariant entry + per new alert | WS-10 | 0.7d |
| T22 | License H7 — 14-day warn-only enforcement + flag | REV-FINAL-LICENSE-COMM + WS-15 | 0.5d |
| T23 | License H7 — customer communication coordination | REV-FINAL-LICENSE-COMM + WS-15/WS-16 | 0.3d |
| T24 | INVESTIGATE-001 close-out (SH1-3) | v2 §99 | 0.2d |

**Total**: ~9.4d (matches v1 §3 estimate 3-4 人日 × ~2 once dashboards/runbooks are counted).

---

## 3. Tasks

---

### T01 — Add startup-Ping helpers in `shared/platform/store`

**1. WHAT IT IS**
- Three new exported functions in the existing `shared/platform/store` package:
  - `PingPG(ctx context.Context, pool *pgxpool.Pool) error` in `pg.go`.
  - `PingRedis(ctx context.Context, rdb redis.UniversalClient) error` in `redis.go`.
  - `PingClickHouse(ctx context.Context, conn driver.Conn) error` in `clickhouse.go`.
- Each function MUST: (a) apply a 5-second timeout, (b) wrap errors with `%w`, (c) return nil on success.

**2. WHAT IT IS NOT**
- ❌ NOT modifying `NewPG` / `NewRedis` / `NewClickHouse` to ping internally. We keep construction lazy so that unit tests with mock drivers do not break. The wiring lives in T02.
- ❌ NOT a health-loop. This is a one-shot startup probe. The periodic `HealthCheckPeriod` on the pgx pool stays untouched (`pg.go:37`).
- ❌ NOT a `/livez` change. T03 handles readiness; livez stays trivial.

**3. WHY THIS EXISTS**
- Root cause: `NewPG` returns from `pgxpool.NewWithConfig` immediately; pgx connects lazily on first `Acquire`. Same shape for Redis (`go-redis` is lazy) and ClickHouse (`clickhouse.Open` lazy).
- Source-verified: `pg.go:21-45`, `redis.go:6-12`, `clickhouse.go:44-46`.
- Symptom: a binary with wrong DSN starts cleanly, `/livez` and `/readyz` return 200, traffic is routed to a doomed pod. First real request fails with a confusing acquire-timeout.
- Origin: audit S1 (sonnet `ab6bb6e04ee3458b7`), master plan v1 §2.2 S1.

**4. DO**
1. Open `shared/platform/store/pg.go` and append `PingPG`.
2. Open `shared/platform/store/redis.go` and append `PingRedis`.
3. Open `shared/platform/store/clickhouse.go` and append `PingClickHouse`.
4. Add unit tests in the existing `*_test.go` files using a context with a 1ms timeout to assert the wrapper actually propagates timeout (negative test).

**5. DON'T**
- ❌ Don't add the Ping calls inside the `New*` constructors. Tests in `tests/contract` and `tests/e2e` build fake stores; auto-pinging breaks them.
- ❌ Don't call `panic` here. Return `error`. T02 decides what panic means.
- ❌ Don't add ping retries. If Ping fails at boot, we want the pod to crash so Kubernetes restarts it. Retry loops mask DNS misconfiguration.
- ❌ Don't reuse `context.Background()`. Always derive from the caller's ctx so shutdown signals propagate.

**6. HOW (concrete)**

`shared/platform/store/pg.go` (append after current line 45):
```go
// PingPG verifies the pool can acquire a connection and round-trip a SELECT 1.
// Use at startup; do not call on the hot path.
func PingPG(ctx context.Context, pool *pgxpool.Pool) error {
    ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
    defer cancel()
    if err := pool.Ping(ctx); err != nil {
        return fmt.Errorf("store.PingPG: %w", err)
    }
    return nil
}
```
Add import `"fmt"`.

`shared/platform/store/redis.go` (append after current line 12):
```go
import (
    "context"
    "fmt"
    "time"

    "github.com/redis/go-redis/v9"
)

// PingRedis verifies the client can complete a PING command within 5s.
func PingRedis(ctx context.Context, rdb redis.UniversalClient) error {
    ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
    defer cancel()
    if err := rdb.Ping(ctx).Err(); err != nil {
        return fmt.Errorf("store.PingRedis: %w", err)
    }
    return nil
}
```

`shared/platform/store/clickhouse.go` (append after current line 46):
```go
// PingClickHouse verifies the driver can complete a Ping within 5s.
func PingClickHouse(ctx context.Context, conn driver.Conn) error {
    ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
    defer cancel()
    if err := conn.Ping(ctx); err != nil {
        return fmt.Errorf("store.PingClickHouse: %w", err)
    }
    return nil
}
```
Add imports `"context"`, `"fmt"` if missing.

**7. VERIFY**
```bash
cd AILinkMax-Go
go build ./shared/platform/store/...
go test -race -count=2 ./shared/platform/store/... -run 'TestPing'
echo "EXIT=$?" | tee logs/WS10-T01-verify.txt
```
Expected: `PASS` for new tests, `EXIT=0` in the log. Negative test (1ms timeout) must return `context.DeadlineExceeded` wrapped under `errors.Is`.

**8. DEPENDENCIES / OWNER**
- Blocks: T02, T03.
- Blocked by: none.
- Owner: Track G (SRE).

---

### T02 — Wire startup Ping into all 5 Go binaries; fail-fast on error

**1. WHAT IT IS**
- Insert a `store.PingPG` / `store.PingRedis` / `store.PingClickHouse` call sequence in each binary `main.go` immediately after store construction and before HTTP listener / worker launch:
  - `binaries/web/main.go`
  - `binaries/all-in-one/main.go`
  - `binaries/worker-billing/main.go`
  - `binaries/worker-archive/main.go`
  - `binaries/worker-scheduler/main.go`
  - `binaries/worker-gatewaycontrol/main.go`
- On any Ping error, log a structured `slog.Error` and `os.Exit(1)` (no panic — exit code stays clean for systemd / k8s).

**2. WHAT IT IS NOT**
- ❌ NOT modifying the wire/wire-gen files (`app/wire.go`); the Ping happens in `main.go` after wire returns.
- ❌ NOT skipping pings in tests. Tests use the in-memory test harness, which never enters `main.go`.
- ❌ NOT pinging stores the binary does not use. `worker-billing` may not need ClickHouse, but **does** need PG and Redis; verify each binary's `app/wire.go` to know which stores it actually constructs.

**3. WHY THIS EXISTS**
- Root cause: see T01 §3. Without binaries actually calling the helpers, T01 is dead code.
- Origin: audit S1.

**4. DO**
1. For each binary, read its `main.go` to find the line where the constructed stores are passed into the app/router.
2. Read its `app/wire.go` to enumerate which of PG / Redis / ClickHouse it actually holds.
3. Immediately above the listener/worker start, call the matching Ping helpers.
4. On error, `slog.Error("store ping failed", "store", "<name>", "err", err)` and `os.Exit(1)`.
5. Pass the binary's root ctx (the one bound to `signal.NotifyContext`) so SIGTERM during boot still cancels.

**5. DON'T**
- ❌ Don't pre-flight every store with a fan-out goroutine; do them sequentially. The total budget is 15s, the failure mode is rare, and serial output is easier to read in `kubectl logs`.
- ❌ Don't swallow the error. `if err != nil { return }` lets the binary continue serving 5xx.
- ❌ Don't call panic. Panic produces a stack trace that operators must `grep`. `os.Exit(1)` with an `slog.Error` is the contract.
- ❌ Don't put the Ping in a `defer` — they only run after main returns.

**6. HOW (concrete)**

Example for `binaries/web/main.go` (adjust per binary):
```go
// after deps := app.New(...) returns (or however the binary names it)
if err := store.PingPG(ctx, deps.PgPool); err != nil {
    slog.Error("store ping failed", "store", "postgres", "err", err)
    os.Exit(1)
}
if err := store.PingRedis(ctx, deps.Rdb); err != nil {
    slog.Error("store ping failed", "store", "redis", "err", err)
    os.Exit(1)
}
if deps.ChConn != nil { // worker-archive uses CH; web may not
    if err := store.PingClickHouse(ctx, deps.ChConn); err != nil {
        slog.Error("store ping failed", "store", "clickhouse", "err", err)
        os.Exit(1)
    }
}
```

Add imports `"os"` and the `store` package alias used elsewhere in the file.

**Binary inventory — fill in by reading each `app/wire.go`** (do this in-PR; do NOT trust this table without verification):
| Binary | PG | Redis | CH |
|---|---|---|---|
| web | yes | yes | no (unless analytics endpoints) |
| all-in-one | yes | yes | yes |
| worker-billing | yes | yes | yes (settlement reads usage:detailed) |
| worker-archive | yes | yes | yes |
| worker-scheduler | yes | yes | no |
| worker-gatewaycontrol | yes | yes | no |

**7. VERIFY**
1. Build all binaries: `go build ./binaries/...`
2. Negative test (manual): set `GO_DB_DSN=postgres://wrong:wrong@127.0.0.1:1/none` and run `./bin/web`. Expected: process exits non-zero within 6s with `slog` line `store ping failed store=postgres`.
3. Positive test: with a healthy dev stack, run `./bin/web` and confirm it transitions to listening on port (existing smoke test in `tests/e2e/smoke_test.go`).
4. Log: `logs/WS10-T02-verify.txt` contains the negative-test stderr line and an exit code.

**8. DEPENDENCIES / OWNER**
- Blocks: T03 (readiness aggregator wants Ping helpers proven first).
- Blocked by: T01.
- Owner: Track G (SRE) + Track E (Senior Go #3 reviews binary edits).

---

### T03 — Real `/readyz` aggregator (PG/Redis/CH + SupervisorRegistry)

**1. WHAT IT IS**
- Replace the trivial `/readyz` handler in `shared/runtime/host.go:46` with one that aggregates:
  - `store.PingPG(ctx, pool)` (per-binary store list).
  - `store.PingRedis(ctx, rdb)`.
  - `store.PingClickHouse(ctx, ch)` if the binary constructs CH.
  - `runtime.SupervisorRegistry.AllHealthy()` (provided by REV-FINAL-V11 / WS-4 — if not yet landed, gate the supervisor check behind `Deps.Supervisors != nil`).
- Each sub-check has a 1-second budget. Total budget 3s.
- On any sub-check failure: respond `503 Service Unavailable` with JSON body `{"status":"unready","failed":["<name>",...]}`.
- `/livez` stays unchanged.

**2. WHAT IT IS NOT**
- ❌ NOT pinging on every kubelet probe at 1Hz when the pool is large — use `pool.Ping` (light SELECT 1), not `pool.Acquire(...).Conn().Ping()`.
- ❌ NOT a deep health endpoint that runs queries against business tables. That belongs to `/healthz/*` (admin).
- ❌ NOT a circuit breaker. Don't cache the result. A flaky check should produce a flaky `/readyz`; that is the desired feedback to k8s.

**3. WHY THIS EXISTS**
- `/readyz` currently lies: returns 200 even when PG is unreachable (`host.go:46`). Result: k8s rolling update drains traffic to a dead replica because readyz says it's ready.
- Origin: audit S1.

**4. DO**
1. Extend `runtime.Deps` (in `shared/runtime/deps.go` if present, else `module.go`) with optional fields: `PgPool *pgxpool.Pool`, `Rdb redis.UniversalClient`, `ChConn driver.Conn`, `Supervisors *SupervisorRegistry`.
2. Change `BuildRouter` to register a new readyz handler that closes over `Deps`.
3. Implement the handler as a small struct with a `ServeHTTP` method (so the timeout / fan-out logic is testable).
4. Add a unit test in `shared/runtime/host_test.go` that injects a fake `pool` returning an error and asserts 503 + JSON body shape.
5. Each Go binary's `main.go` populates the new `Deps` fields before calling `BuildRouter`.

**5. DON'T**
- ❌ Don't run the sub-checks in parallel with unbounded `go func`. Use a `errgroup.WithContext` with the 3s timeout; child cancellations propagate.
- ❌ Don't include `time.Sleep` to "smooth" flapping. Real flapping means readyz must flap.
- ❌ Don't include internal hostnames or DSNs in the JSON body. Only the short name ("postgres", "redis", "clickhouse", "supervisors").
- ❌ Don't return 200 when any check is `nil` because the binary didn't wire it in. If a binary needs CH and forgets to wire it, that's a bug — 503 surfaces it.

**6. HOW (concrete)**

`shared/runtime/host.go` replace the existing line 46:
```go
r.Get("/readyz", buildReadyzHandler(d))
```

Add (new helper inside the same file or a new `readyz.go`):
```go
func buildReadyzHandler(d Deps) http.HandlerFunc {
    return func(w http.ResponseWriter, req *http.Request) {
        ctx, cancel := context.WithTimeout(req.Context(), 3*time.Second)
        defer cancel()

        type result struct {
            name string
            err  error
        }
        var checks []func() result
        if d.PgPool != nil {
            checks = append(checks, func() result {
                return result{"postgres", store.PingPG(ctx, d.PgPool)}
            })
        }
        if d.Rdb != nil {
            checks = append(checks, func() result {
                return result{"redis", store.PingRedis(ctx, d.Rdb)}
            })
        }
        if d.ChConn != nil {
            checks = append(checks, func() result {
                return result{"clickhouse", store.PingClickHouse(ctx, d.ChConn)}
            })
        }
        if d.Supervisors != nil {
            checks = append(checks, func() result {
                if d.Supervisors.AllHealthy() {
                    return result{"supervisors", nil}
                }
                return result{"supervisors", errors.New("one or more supervisors unhealthy")}
            })
        }

        var failed []string
        for _, c := range checks {
            r := c()
            if r.err != nil {
                failed = append(failed, r.name)
            }
        }

        w.Header().Set("Content-Type", "application/json; charset=utf-8")
        if len(failed) > 0 {
            w.WriteHeader(http.StatusServiceUnavailable)
            _ = json.NewEncoder(w).Encode(map[string]any{"status": "unready", "failed": failed})
            return
        }
        w.WriteHeader(http.StatusOK)
        _ = json.NewEncoder(w).Encode(map[string]any{"status": "ready"})
    }
}
```
Add imports `"encoding/json"`, `"errors"`, `"time"`, `"context"`, the store package, the redis client interface type, the pgxpool type, the clickhouse driver type.

**Soft dependency on WS-4**: if `SupervisorRegistry` from REV-FINAL-V11 has not landed, leave `d.Supervisors` nil in every binary. The readyz handler degrades gracefully.

**7. VERIFY**
1. Unit: `go test -race ./shared/runtime/... -run TestReadyz` — assert 200 with all healthy stubs, assert 503 with one failing stub.
2. Integration (manual): kill the dev Postgres container, hit `curl -s -o - -w '%{http_code}\n' localhost:8000/readyz`. Expected: `503 ... "failed":["postgres"]`.
3. e2e: `tests/e2e/smoke_test.go:26` already hits `/readyz` — ensure it still returns 200 under the healthy dev stack.
4. Log to `logs/WS10-T03-verify.txt`.

**8. DEPENDENCIES / OWNER**
- Blocks: license middleware T22 (license check sits after readyz in middleware order, per REV-FINAL-V13).
- Blocked by: T01, T02. Soft dep on WS-4 SupervisorRegistry (REV-FINAL-V11).
- Owner: Track G (SRE).

---

### T04 — `httplog` redaction helpers (path + UA + headers)

**1. WHAT IT IS**
- New file `shared/runtime/httplog_redact.go` exporting:
  - `RedactPath(raw string) string` — masks sensitive path segments (e.g. `/api/v1/users/<email>/...` → `/api/v1/users/[redacted]/...`).
  - `RedactUserAgent(raw string) string` — collapses tokens that match the sensitive regex catalog to `[redacted]`.
  - `RedactHeader(name, value string) string` — Authorization / X-Api-Key / Cookie always returns `[redacted]`; other headers returned verbatim.
- Helpers MUST be pure (no allocations on the no-match fast path beyond strings.Index).

**2. WHAT IT IS NOT**
- ❌ NOT a generic PII scrubber. Scope is restricted to the three call sites in `Observe` (path / UA) plus header logging that WS-5 will add.
- ❌ NOT removing the `request_id`, `method`, `status`, `latency_ms`, `bytes` fields. Those stay verbatim — they are the audit log.
- ❌ NOT logging the request body. Body redaction is out of scope (Pingora handles wire-fidelity archive).

**3. WHY THIS EXISTS**
- `httplog.go:61-62` writes `r.URL.Path` (which may contain emails / API keys when clients mis-configure SDKs) and `r.RemoteAddr` (which is fine — it is the network peer, not PII). Line 102 writes `r.UserAgent()` (which often embeds `sk-...` because clients accidentally paste the key into a custom User-Agent header during debugging).
- Sample of real-world UA values from staging logs (per audit S7): `"axios/0.27.2 Bearer sk-abcdef..."`, `"Custom: api_key=sk-..."` — none should ever reach disk.
- Origin: audit S7 (sonnet `ab6bb6e04ee3458b7`).

**4. DO**
1. Write the helpers + tests **first** (TDD).
2. Define the sensitive-regex catalog as `var sensitiveTokenRegexps = []*regexp.Regexp{ ... }` compiled at package init; expose `SensitivePatterns() []string` for the lint script (T06) to consume.
3. Cover with unit tests: positive (each regex matches), negative (innocuous strings pass through), perf (no panic / no allocation on 1KB random ASCII input — use `testing.AllocsPerRun`).

**5. DON'T**
- ❌ Don't use a global `regexp` mutation. The catalog is read-only after init.
- ❌ Don't use the standard `regexp` package's `RE2` syntax for any catastrophic-backtracking constructs — `regexp` is RE2 and already linear, so OK; but keep regexes short and anchored where possible.
- ❌ Don't redact the **whole** path. Operators need the route shape for debugging. Mask only segments matching `[A-Za-z0-9+/=._-]{16,}` that look like tokens, plus email shapes.
- ❌ Don't redact `request_id` (it is server-allocated and must remain searchable).

**6. HOW (concrete)**

File `shared/runtime/httplog_redact.go`:
```go
package runtime

import (
    "regexp"
    "strings"
)

const RedactedMarker = "[redacted]"

// sensitiveTokenRegexps matches substrings that must never reach logs.
// Order matters only for the lint script's reporting; matching is independent.
var sensitiveTokenRegexps = []*regexp.Regexp{
    regexp.MustCompile(`sk_(live|test)_[A-Za-z0-9]{8,}`),       // Stripe-shaped secret keys
    regexp.MustCompile(`sk-[A-Za-z0-9]{20,}`),                  // OpenAI / Anthropic style API keys
    regexp.MustCompile(`(?i)bearer\s+[A-Za-z0-9._\-]{12,}`),    // Bearer JWT / opaque tokens
    regexp.MustCompile(`(?i)authorization:\s*[^\s]+`),          // Inline header dumps
    regexp.MustCompile(`(?i)x-api-key[:=]\s*[A-Za-z0-9._\-]+`), // Inline header dumps
    regexp.MustCompile(`[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}`), // emails
    regexp.MustCompile(`eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}`), // JWT
}

// SensitivePatterns returns the regex strings for the CI lint script.
func SensitivePatterns() []string {
    out := make([]string, len(sensitiveTokenRegexps))
    for i, r := range sensitiveTokenRegexps {
        out[i] = r.String()
    }
    return out
}

// RedactPath replaces tokenlike segments with [redacted]. Route shape is preserved.
func RedactPath(raw string) string {
    if raw == "" {
        return raw
    }
    segs := strings.Split(raw, "/")
    for i, s := range segs {
        if s == "" {
            continue
        }
        masked := s
        for _, r := range sensitiveTokenRegexps {
            masked = r.ReplaceAllString(masked, RedactedMarker)
        }
        segs[i] = masked
    }
    return strings.Join(segs, "/")
}

// RedactUserAgent collapses every sensitive token match in the UA string.
func RedactUserAgent(raw string) string {
    if raw == "" {
        return raw
    }
    out := raw
    for _, r := range sensitiveTokenRegexps {
        out = r.ReplaceAllString(out, RedactedMarker)
    }
    return out
}

// RedactHeader returns [redacted] for known-sensitive headers, verbatim otherwise.
func RedactHeader(name, value string) string {
    switch strings.ToLower(name) {
    case "authorization", "cookie", "x-api-key", "x-api-key-id", "proxy-authorization":
        if value == "" {
            return value
        }
        return RedactedMarker
    default:
        return value
    }
}
```

Tests in `shared/runtime/httplog_redact_test.go` cover each regex + `RedactHeader` switch + path segment preservation.

**7. VERIFY**
```bash
go test -race -count=2 ./shared/runtime/... -run 'TestRedact'
go test -bench=BenchmarkRedactUserAgent -benchmem ./shared/runtime/... | tee logs/WS10-T04-bench.txt
echo "EXIT=$?" | tee -a logs/WS10-T04-verify.txt
```
Expected: PASS; benchmark allocs/op ≤ 4 for 100B inputs without sensitive tokens; sample assertion `RedactPath("/api/v1/users/me@example.com/keys") == "/api/v1/users/[redacted]/keys"`.

**8. DEPENDENCIES / OWNER**
- Blocks: T05, T06.
- Blocked by: none.
- Owner: Track G (SRE).

---

### T05 — Wire redaction into `Observe` middleware + plog seed

**1. WHAT IT IS**
- Modify `shared/runtime/httplog.go` to call `RedactPath` / `RedactUserAgent` before logging.
- Two call sites:
  - Line 61: `"path", r.URL.Path` → `"path", RedactPath(r.URL.Path)`.
  - Line 102: `"user_agent", r.UserAgent()` → `"user_agent", RedactUserAgent(r.UserAgent())`.
- `r.RemoteAddr` stays verbatim (network peer, not PII).

**2. WHAT IT IS NOT**
- ❌ NOT replacing the `plog` package; we still seed the same logger into ctx.
- ❌ NOT redacting in `panic` recovery output beyond what's already covered — the `fmt.Sprint(rec)` value can in theory contain a secret, but the upstream WS-5 fail-closed work is the place to guarantee that. T05 only handles request-line fields.

**3. WHY THIS EXISTS**
- See T04 §3. Without this wiring T04 is dead code.

**4. DO**
1. Edit `httplog.go:61` and `httplog.go:102`.
2. Re-run the existing `httplog_test.go` to make sure assertions still hold (they should — the test uses static paths/UAs).
3. Add a new test case `TestObserveRedactsSensitivePath` that issues a request to `/api/v1/users/sk-AAAAAAAAAAAAAAAAAAAA/x` and asserts the captured log line contains `[redacted]`, not the token.

**5. DON'T**
- ❌ Don't pre-compute `RedactPath(r.URL.Path)` twice — assign to a local `loggedPath` once near the top.
- ❌ Don't move the `plog` initialisation outside the request scope.
- ❌ Don't change the chi middleware order from `RequestID → RealIP → securityHeaders → limitRequestBody → rateLimiter → Observe`. The redaction is downstream of `RequestID`, which is correct.

**6. HOW (concrete)**

Edit `httplog.go` lines 57-63:
```go
reqID := middleware.GetReqID(r.Context())
loggedPath := RedactPath(r.URL.Path)
rl := base.With(
    "request_id", reqID,
    "method", r.Method,
    "path", loggedPath,
    "ip", r.RemoteAddr,
)
```

Edit `httplog.go` around line 98-103:
```go
lg := rl.With(
    "status", status,
    "latency_ms", latencyMs,
    "bytes", sr.bytes,
    "user_agent", RedactUserAgent(r.UserAgent()),
)
```

**7. VERIFY**
```bash
go test -race -count=2 ./shared/runtime/... -run 'TestObserve'
```
Expected: PASS. The new assertion `TestObserveRedactsSensitivePath` must fail before the edits and pass after. Capture in `logs/WS10-T05-verify.txt`.

**8. DEPENDENCIES / OWNER**
- Blocks: T06 (lint script greps for unredacted call sites).
- Blocked by: T04.
- Owner: Track G (SRE).

---

### T06 — Sensitive-pattern regex catalog + CI lint

**1. WHAT IT IS**
- New script `scripts/check-log-redaction.sh` (and mirror `scripts/check-log-redaction.ps1` per CLAUDE.md "scripts/*.sh + scripts/*.ps1 must not drift").
- Behaviour: grep the repo for `slog.Info|slog.Warn|slog.Error|slog.Debug|log.Printf` calls that take `r.URL.Path`, `r.UserAgent()`, `r.Header.Get`, or string-literal headers (`"Authorization"`, etc.) without going through `runtime.RedactPath` / `RedactUserAgent` / `RedactHeader`.
- Exit 0 if zero offenders; exit 1 with line-numbered report otherwise.
- Wire into `make check`.

**2. WHAT IT IS NOT**
- ❌ NOT a full taint analysis. Approximate; false positives are acceptable and are silenced with a single-line annotation `// log-redact: ok — <reason>`.
- ❌ NOT redaction of structured fields beyond the three helpers. Anything new must go through them.
- ❌ NOT a separate tool — it's a shell script. We do not introduce a Go AST walker here (that's WS-12 territory for the supervisor lint; the redaction lint stays simple).

**3. WHY THIS EXISTS**
- T05 fixes the known call sites. Without a lint, a future PR can add `slog.Info("incoming", "path", r.URL.Path)` in any module and recreate S7.
- Origin: audit S7 + master plan v1 WS-10 acceptance criterion "0 个明显 secret pattern".

**4. DO**
1. Write the .sh script that:
   - Walks `binaries/`, `modules/`, `shared/`.
   - For each `*.go` file, runs a literal grep for the offender patterns listed below.
   - Excludes lines containing `// log-redact: ok`.
   - Excludes the `shared/runtime/httplog*.go` and `httplog_redact*.go` files (they are the definition site).
2. Hook into `make check` after `golangci-lint`.
3. Write the .ps1 mirror.

**5. DON'T**
- ❌ Don't use `find ... -exec grep`; that gives unsortable output. Use `grep -RnE --include='*.go'`.
- ❌ Don't try to verify regex coverage of all patterns from `SensitivePatterns()` — that is T04's unit test territory.
- ❌ Don't ignore violations in vendor/ — vendor is git-ignored anyway, but be explicit.

**6. HOW (concrete)**

`scripts/check-log-redaction.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Patterns that indicate a likely unredacted secret being logged.
PATTERNS=(
  'r\.URL\.Path'
  'r\.UserAgent\(\)'
  'r\.Header\.Get\("Authorization"\)'
  'r\.Header\.Get\("Cookie"\)'
  'r\.Header\.Get\("X-Api-Key"\)'
)

DIRS=(binaries modules shared)
FAIL=0

for p in "${PATTERNS[@]}"; do
  while IFS=: read -r f line content; do
    case "$f" in
      shared/runtime/httplog*.go) continue ;;
    esac
    if grep -q '// log-redact: ok' <<<"$content"; then
      continue
    fi
    # Allow lines that already wrap through Redact*
    if grep -q 'Redact\(Path\|UserAgent\|Header\)' <<<"$content"; then
      continue
    fi
    echo "OFFENDER $f:$line: $content"
    FAIL=1
  done < <(grep -RnE "$p" --include='*.go' "${DIRS[@]}" || true)
done

if [ "$FAIL" -ne 0 ]; then
  echo "scripts/check-log-redaction.sh: unredacted secret-bearing log call(s) found."
  exit 1
fi
echo "scripts/check-log-redaction.sh: OK"
```

Add `chmod +x scripts/check-log-redaction.sh` and a `Makefile` entry:
```makefile
check: ... check-log-redaction
check-log-redaction:
	bash scripts/check-log-redaction.sh
```

PowerShell mirror writes the same patterns using `Select-String -Pattern` per CLAUDE.md "scripts/*.ps1 not drift".

**7. VERIFY**
```bash
bash scripts/check-log-redaction.sh   # expect: OK exit 0
# Negative: add `slog.Info("x", "path", r.URL.Path)` in a throwaway file, rerun → expect exit 1.
make check                            # full suite still green
```
Log to `logs/WS10-T06-verify.txt`.

**8. DEPENDENCIES / OWNER**
- Blocks: master plan §7 #6 (CI lint) acceptance.
- Blocked by: T04, T05.
- Owner: Track G (SRE).

---

### T07 — Kill `request_id` Prom label in Rust (R15 inflight_reaper)

**1. WHAT IT IS**
- Edit `crates/gw-ledger/src/inflight_reaper.rs:119-124` to drop the `request_id` label from `gw_billing_emit_lag_t0_total`.
- The `attempt_index` label stays (bounded: small integer).
- The `request_id` value remains in the structured `tracing::error!` log (line 114) — logs are not subject to label cardinality.

**2. WHAT IT IS NOT**
- ❌ NOT removing the `tracing::error!` log — operators still need `request_id` for correlation when investigating an actual lag.
- ❌ NOT touching `detailed_archive.rs:222` — that is a Redis stream XADD field (`("request_id", ...)` is data, not a Prom label). Verified by plan author.
- ❌ NOT changing the metric name (would break dashboards more than necessary; rely on T09/T10 to migrate the queries).

**3. WHY THIS EXISTS**
- Prometheus stores one time series per unique label set. `request_id` is a UUID-shaped string with effectively unbounded cardinality. At 100 req/s, the Prom WAL grows ~8 MB/h, and `/metrics` scrape size grows linearly with retention. v1 §2.3 R15 estimate: "scrape 几小时撑爆 Prom".
- Origin: audit R15 (sonnet `ab9a0b56c409b36bd`).

**4. DO**
1. Edit lines 119-124 of `crates/gw-ledger/src/inflight_reaper.rs`:
   - Remove `"request_id" => request_id.to_string(),`.
   - Keep `"attempt_index" => attempt.to_string(),`.
2. Update the unit test in `crates/gw-ledger/tests/inflight_reaper.rs` if it asserts on the label set.
3. Add a regression test that scrapes the metrics registry after triggering the lag path and verifies only `attempt_index` is present.

**5. DON'T**
- ❌ Don't replace `request_id` with a hashed bucket. Bucketing collisions confuse operators; either keep the data in logs (tracing) or in exemplars (T09 covers exemplars).
- ❌ Don't move the metric to a histogram with a `request_id` exemplar — exemplars are stored separately, but the Prom client crate's exemplar API in `metrics 0.21+` is unstable. Stick to plain counter with bounded labels; exemplar uplift is a follow-up in T09.
- ❌ Don't delete the metric. It's referenced by the v1 §4 WS-1 reconciler design.

**6. HOW (concrete)**

`crates/gw-ledger/src/inflight_reaper.rs` lines 119-124:
```rust
metrics::counter!(
    "gw_billing_emit_lag_t0_total",
    "attempt_index" => attempt.to_string(),
)
.increment(1);
```

**7. VERIFY**
```bash
cd ailinkmax-gateway
cargo test -p gw-ledger -- --ignored inflight_reaper
cargo build --release -p gw-gateway
# Spin up dev gateway, hit /metrics, grep:
curl -s localhost:9090/metrics | grep gw_billing_emit_lag_t0_total | tee logs/WS10-T07-verify.txt
```
Expected: each line has exactly `attempt_index="<n>"` and **no** `request_id="..."`.

**8. DEPENDENCIES / OWNER**
- Blocks: T08 (lint), T09 (dashboard migration), T10 (alert migration).
- Blocked by: none.
- Owner: Track G (SRE) coordinating with Track B/C (Rust owners).

---

### T08 — CI lint: forbid high-cardinality metric labels

**1. WHAT IT IS**
- New script `scripts/check-metric-labels.sh` (mirror `.ps1`) in `ailinkmax-gateway/`.
- Scans `crates/**/*.rs` for `metrics::counter!|metrics::gauge!|metrics::histogram!` invocations.
- Flags any label whose value is **not** in the enum allowlist defined inline in the script:
  - Allowed: literal strings (`"path" => "channel"`), `attempt_index`, `outcome`, `kind`, `path`, `vendor`, `encoding`, `surface`, `from`, `to`, `disabled_now`, `protocol`.
  - Disallowed (with explicit kill-list): `request_id`, `api_key_id`, `user_id`, `trace_id` (when used as a Prom label — XADD usage in `detailed_archive.rs` is excluded by file path).
- Exit 1 with line-numbered offenders.

**2. WHAT IT IS NOT**
- ❌ NOT a syn-based AST analyzer (REV-FINAL-V9 calls for AST elsewhere; we keep this one as a focused grep to avoid blocking T08 on the larger CI infra).
- ❌ NOT removing the `request_id` stream field in `detailed_archive.rs:222`. The script must exclude `detailed_archive.rs` from the `request_id` rule because that file uses it as a Redis XADD field, not a Prom label.

**3. WHY THIS EXISTS**
- Per REV-FINAL-V7: "metric label value 必须 enum allowlist". Today there is no enforcement; a future commit can re-add `request_id` and only show up as an outage when Prom OOMs.
- Origin: audit R15 + master plan §7 #6.

**4. DO**
1. Author the script. Pattern:
   - `grep -RnE '(metrics::counter!|metrics::gauge!|metrics::histogram!)\([^)]*"request_id"' crates/`.
   - Filter out `crates/gw-ledger/src/detailed_archive.rs` (XADD usage).
2. Add to `scripts/check-invariants.sh` (existing umbrella) so it runs in `make e2e` and CI.
3. Document the allowlist in `docs/architecture.md` §Invariants (per CLAUDE.md sync rule).

**5. DON'T**
- ❌ Don't try to parse the macro perfectly with regex. False positives are acceptable; users add `// metric-label: ok` to silence.
- ❌ Don't extend the allowlist by editing the script in a feature PR — that defeats the purpose. Allowlist additions require their own PR with operator sign-off.

**6. HOW (concrete)**

`scripts/check-metric-labels.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DISALLOWED=( request_id api_key_id user_id trace_id )
EXEMPT_FILES=( crates/gw-ledger/src/detailed_archive.rs )

FAIL=0
for label in "${DISALLOWED[@]}"; do
  while IFS=: read -r f line content; do
    skip=0
    for e in "${EXEMPT_FILES[@]}"; do
      [ "$f" = "$e" ] && skip=1
    done
    [ "$skip" = 1 ] && continue
    if grep -q '// metric-label: ok' <<<"$content"; then
      continue
    fi
    # Only flag inside metrics::counter!|gauge!|histogram!
    echo "OFFENDER $f:$line label=$label : $content"
    FAIL=1
  done < <(grep -RnE "metrics::(counter|gauge|histogram)!\([^)]*\"$label\"" --include='*.rs' crates/ || true)
done

if [ "$FAIL" -ne 0 ]; then
  echo "scripts/check-metric-labels.sh: high-cardinality label in Prom metric"
  exit 1
fi
echo "scripts/check-metric-labels.sh: OK"
```

**7. VERIFY**
```bash
bash scripts/check-metric-labels.sh    # expect OK
# Negative: revert T07 locally and rerun → expect OFFENDER report and exit 1.
```
Log to `logs/WS10-T08-verify.txt`.

**8. DEPENDENCIES / OWNER**
- Blocks: master plan §7 #6 acceptance.
- Blocked by: T07.
- Owner: Track G (SRE).

---

### T09 — Grafana dashboard migration (delete `request_id` panels, replace with exemplars / `attempt_index`)

**1. WHAT IT IS**
- Audit and migrate every Grafana panel and Prometheus query that references `request_id` as a label.
- Output: a single PR that ships new dashboard JSON under `deploy/observability/dashboards/` (or wherever existing dashboards live — read `deploy/` first to confirm).
- Each migrated panel: drop the `request_id` filter, either (a) aggregate over `attempt_index` if the panel is a rate/sum, or (b) replace with a tracing-link annotation that points to logs filtered by `request_id`.

**2. WHAT IT IS NOT**
- ❌ NOT a redesign of the dashboards' visual story. Same panels, just no `request_id` label.
- ❌ NOT migrating dashboards that don't reference `request_id`.
- ❌ NOT a Grafana version bump.

**3. WHY THIS EXISTS**
- REV-FINAL-R15-obs: "删除 v1 中 R15 高基数 label 的同时迁移现有 Grafana dashboard 与 alert，避免静默失声".
- Without dashboard migration, T07's label kill silently breaks panels that filter `{request_id=~".*"}` — operators see empty graphs.

**4. DO**
1. Enumerate dashboard files: `find deploy -name '*.json'` + check `ailinkmax-gateway/deploy/`.
2. `grep -lE 'request_id' deploy/observability/dashboards/*.json` → list of dashboards to migrate.
3. For each dashboard, edit each panel that references `request_id`:
   - If the query is `sum(rate(gw_billing_emit_lag_t0_total{request_id=~"$req"}[5m]))` → rewrite as `sum by (attempt_index) (rate(gw_billing_emit_lag_t0_total[5m]))`.
   - Add a panel description note: "Per-request drill-down: see Loki/Tempo trace `request_id={{var}}`".
4. Verify in a local Grafana that each migrated panel renders with no errors when scraping the post-T07 gateway.

**5. DON'T**
- ❌ Don't replace `request_id` with a hash bucket. That recreates a cardinality problem (1k buckets × 10 panels = 10k series).
- ❌ Don't delete dashboards even if they look unused — they may be ad-hoc operator favourites. Just migrate.
- ❌ Don't commit dashboard JSON with `"editable": true` flips toggled (creates noise in PRs).

**6. HOW (concrete)**
- Use `jq` to scope edits:
  ```bash
  for f in deploy/observability/dashboards/*.json; do
    jq '.. | objects | select(.expr? | tostring | test("request_id"))' "$f"
  done
  ```
- Replace `request_id="..."` filter expressions in `.panels[].targets[].expr` strings.
- Add a `links` entry on the panel pointing to Loki search `{request_id="$req"}`.
- Save formatted JSON: `jq --indent 2 . < in.json > out.json`.

**7. VERIFY**
1. Re-run `grep -RlE '"request_id"' deploy/observability/dashboards/` — expect empty output.
2. Spin up local Grafana via `docker-compose -f deploy/observability/docker-compose.yml up`; load each migrated dashboard; confirm no "No data" panels with a healthy gateway under load.
3. Log to `logs/WS10-T09-verify.txt` with screenshots paths.

**8. DEPENDENCIES / OWNER**
- Blocks: T10 (alerts).
- Blocked by: T07.
- Owner: Track G (SRE).

---

### T10 — Alert-rule migration (rewrite `request_id`-dependent queries)

**1. WHAT IT IS**
- Audit and migrate every Prometheus alert rule that references `request_id`.
- Output: PR modifying `deploy/observability/alerts/*.yaml` (or equivalent).
- Each migrated rule: rewrite expression to use bounded labels; add `for:` clause if removed.

**2. WHAT IT IS NOT**
- ❌ NOT touching alert routing / Alertmanager config.
- ❌ NOT migrating alerts unrelated to the R15 label kill.

**3. WHY THIS EXISTS**
- REV-FINAL-R15-obs: alerts that filter by `request_id` will silently never fire after T07.
- Origin: audit R15 + REV-FINAL-R15-obs.

**4. DO**
1. `grep -RnE 'request_id' deploy/observability/alerts/` → enumerate rules.
2. For each rule, rewrite the `expr:` to drop `request_id` filtering / grouping.
3. Add an annotation: `description: "drill-down: search logs with request_id={{$labels.attempt_index}}"`.
4. Re-test with `promtool test rules` if test files exist; otherwise add minimal smoke test.

**5. DON'T**
- ❌ Don't introduce new alerts here — that is T17-T20's scope.
- ❌ Don't widen alert `for:` windows to compensate for "less specific" queries unless the SRE on-call signs off.

**6. HOW (concrete)**
- Use `yq` to scope edits. Example pre-/post-:
  - Before: `expr: 'gw_billing_emit_lag_t0_total{request_id!=""} > 0'`
  - After:  `expr: 'sum by (attempt_index) (rate(gw_billing_emit_lag_t0_total[5m])) > 0'`
- Run `promtool check rules deploy/observability/alerts/*.yaml`.

**7. VERIFY**
```bash
grep -RnE 'request_id' deploy/observability/alerts/  # expect empty
promtool check rules deploy/observability/alerts/*.yaml | tee logs/WS10-T10-verify.txt
```
Expected: `SUCCESS` per file.

**8. DEPENDENCIES / OWNER**
- Blocks: master plan §7 #13 (runbook completeness).
- Blocked by: T07, T09.
- Owner: Track G (SRE).

---

### T11 — Prometheus dashboard: WS-1 outbox invariants

**1. WHAT IT IS**
- New Grafana dashboard `deploy/observability/dashboards/ws01-outbox.json`.
- Panels:
  1. `idempotency_keys` row count (rate of inserts / day, total / month).
  2. `payment_outbox` pending count / age oldest pending.
  3. `refund_outbox` pending count / age oldest pending.
  4. `subscription_upgrade_outbox` pending count (REV-FINAL-M10).
  5. Rust `gw_billing_settled_total{outcome}`.
  6. Rust `gw_billing_emit_lag_t0_total` (post-T07 labelset).
  7. Rust `gw_billing_backlog_pending_count` / `_oldest_age_seconds` (existing).
  8. Rust `gw_billing_flusher_panic_total` (existing).

**2. WHAT IT IS NOT**
- ❌ NOT a Go business metrics dashboard (that lives elsewhere).
- ❌ NOT a dashboard that filters by `request_id`.
- ❌ NOT a placeholder — every panel must render with a healthy dev gateway. Use a `make e2e-soak-smoke` run as the baseline data source.

**3. WHY THIS EXISTS**
- Master plan §4 WS-10 关键设计: "新增 Prom dashboard 覆盖：WS-1..9 每个不变式都有 gauge / counter".
- WS-1 invariants (outbox liveness, idem-key writes, settled count) have no operator-facing surface today.

**4. DO**
1. Enumerate which metrics each WS-1 task emits (cross-reference master plan §4 WS-1 task list).
2. Build the JSON dashboard using the Grafana UI on a dev instance, export, format with `jq --indent 2`.
3. Commit to `deploy/observability/dashboards/ws01-outbox.json`.
4. Register in `deploy/observability/grafana-provisioning.yaml` (or wherever existing dashboards are registered).

**5. DON'T**
- ❌ Don't use any deprecated `interval`/`time_from` fields.
- ❌ Don't reuse the same panel ID as an existing dashboard (Grafana provisioning conflicts).

**6. HOW (concrete)**
- Metric names to source (from master plan §4 WS-1):
  - Go: `payment_outbox_pending_count`, `refund_outbox_pending_count`, `idempotency_keys_inserted_total` (new — Track A1/A2 owners must add when implementing T1-T5).
  - Rust: `gw_billing_settled_total`, `gw_billing_backlog_*`, `gw_billing_emit_lag_t0_total`, `gw_billing_flusher_panic_total`, `gateway_freeze_leak_recovered_total`.
- Cross-reference each metric exists in the running gateway before committing the dashboard panel.

**7. VERIFY**
```bash
docker-compose -f deploy/observability/docker-compose.yml up -d
# Run e2e-soak-smoke for live data
( cd ailinkmax-gateway && make e2e-soak-smoke )
# Open Grafana, load ws01-outbox.json, screenshot each panel.
ls deploy/observability/dashboards/ws01-outbox.json | tee logs/WS10-T11-verify.txt
```
Expected: every panel renders non-empty data; no `No data` text.

**8. DEPENDENCIES / OWNER**
- Blocks: T17, T18 (alerts reference these dashboards).
- Blocked by: WS-1 metric emission tasks (T1, T3, T4 must emit named metrics). If not yet landed, ship the dashboard with placeholder targets and update in a follow-up.
- Owner: Track G (SRE) + Track A1/A2 (Go owners) for metric naming.

---

### T12 — Prometheus dashboard: WS-2 worker/drainer invariants

**1. WHAT IT IS**
- New dashboard `deploy/observability/dashboards/ws02-workers.json`.
- Panels:
  1. `gw_tunnel_active_streams` (per node).
  2. Tunnel permit gauge (REV-FINAL-W8 close-await).
  3. Stream PEL size per consumer group (`gw_stream_pel_size{stream}` — new metric for WS-2 S2.18).
  4. Worker supervisor restart counters per name.
  5. `tasks_pending` reconciler claim rate.
  6. `gateway_usage_channel_full_total` (existing).
  7. CH async insert failures.

**2-8.** Same structure as T11 — read master plan §4 WS-2 task list to inventory metric names; commit JSON; verify under `e2e-soak-smoke`.
- Owner: Track G coordinating with Track D (Rust tunnel).

---

### T13 — Prometheus dashboard: WS-3 bounded-memory invariants

**1. WHAT IT IS**
- `deploy/observability/dashboards/ws03-memory.json`.
- Panels:
  1. Per-buffer `*_buffer_bytes_in_use` gauges.
  2. Per-buffer `*_buffer_drop_oldest_total` counters.
  3. `gw_routing_<name>_size` and `gw_routing_<name>_evicted_total` (one row per routing map — InFlightStore, KeyHealthStore, RoundRobin, WeightedRr).
  4. `daily_tokens:*` TTL audit (gauge: `redis_keys_without_ttl_total`).
  5. `BoundedBuf::policy{policy="RejectRequest|Truncate|DropOldest"}` distribution.

**2-8.** Same structure as T11. Owner: Track G + Track C/D.

---

### T14 — Prometheus dashboard: WS-4 supervisor invariants

**1. WHAT IT IS**
- `deploy/observability/dashboards/ws04-supervisor.json`.
- Panels:
  1. `*_panic_recovered_total` for every supervised worker (Go + Rust).
  2. `*_supervisor_restart_total{name, reason}`.
  3. `runtime_supervisor_unhealthy{name}` gauge.
  4. Goroutine count / Tokio task count gauges.

**2-8.** Same structure. Owner: Track G + Track E.

---

### T15 — Prometheus dashboard: WS-5 fail-closed invariants

**1. WHAT IT IS**
- `deploy/observability/dashboards/ws05-authz.json`.
- Panels:
  1. License middleware: `license_block_total{reason}`.
  2. JWT: `gw_auth_jwt_failures_total{reason}`.
  3. HMAC nonce: `gw_auth_replay_detected_total`.
  4. apikey cache mutation invalidation: `apikey_cache_invalidate_total{outcome}`.
  5. mgmttoken Blacklist Revoke errors.
  6. R20 `gw_counter_malformed_total`.

**2-8.** Same structure. Owner: Track G + Track F1/F2.

---

### T16 — Prometheus dashboard: WS-6/7/8 invariants

**1. WHAT IT IS**
- `deploy/observability/dashboards/ws6-7-8-protocol-contract.json`.
- Panels:
  1. R7/R24: `gw_upstream_protocol_decode_error_total{vendor}`.
  2. R8: `gw_response_retry_state_reset_total{field}`.
  3. WS-7: `gw_health_cooldown_clamp_total{key, vendor}` (REV-FINAL-H7.6).
  4. WS-8: rustpact `_pv` mismatch counter.
  5. Cross-protocol render activity (existing).

**2-8.** Same structure. Owner: Track G + Track C.

---

### T17 — Alert rule: `*_panic_recovered_total > 0` (5m)

**1. WHAT IT IS**
- New Prometheus alert rule file `deploy/observability/alerts/ws10-panic.yaml`.
- Rule body fires when any panic counter increases over 5 minutes.

**2. WHAT IT IS NOT**
- ❌ NOT a page-on-every-panic. We use `for: 1m` so transient noise is filtered, but increase over 5m triggers.
- ❌ NOT scoped to a single binary — the rule pattern matches across all `*_panic_recovered_total` metrics so new supervised workers inherit it automatically.

**3. WHY THIS EXISTS**
- Master plan §4 WS-10 关键设计: "新增 alert 规则：`gw_*_panic_recovered > 0` 5min".
- Without it, a worker that panics and restarts keeps the binary up — operators never learn unless they happen to read dashboards.

**4. DO**
1. Inventory all metrics matching `*_panic_recovered_total` across Go + Rust.
2. Write the rule.
3. Add to alerts directory.
4. Test with `promtool`.

**5. DON'T**
- ❌ Don't add `severity: critical` — supervisor panics that get caught are by definition not customer-facing. Severity is `warning`.
- ❌ Don't include `request_id` in annotations (T10 covers the principle, but easy to forget).

**6. HOW (concrete)**
```yaml
groups:
- name: ws10-panic
  rules:
  - alert: SupervisedWorkerPanicRecovered
    expr: |
      sum by (job, name) (
        increase({__name__=~".*_panic_recovered_total"}[5m])
      ) > 0
    for: 1m
    labels:
      severity: warning
      ws: ws-04
    annotations:
      summary: "Worker {{$labels.name}} on {{$labels.job}} recovered from panic"
      runbook: "https://runbooks.internal/alm/ws10-panic"
      description: "Check supervisor restart counter; correlate logs by job + name (NOT request_id)."
```

**7. VERIFY**
```bash
promtool check rules deploy/observability/alerts/ws10-panic.yaml | tee logs/WS10-T17-verify.txt
```
Expected: `SUCCESS`.

**8. DEPENDENCIES / OWNER**
- Blocks: T21 (runbook).
- Blocked by: T14 (supervisor dashboard).
- Owner: Track G.

---

### T18 — Alert rule: `idempotency_keys_reserved_age_seconds` (5m)

**1. WHAT IT IS**
- Alert when the oldest reserved idempotency key (post-WS-1 T1) is older than 5 minutes — indicates the wallet tx is stuck or the row is orphaned (which the WS-1 design says cannot happen, so this is the canary).

**2. WHAT IT IS NOT**
- ❌ NOT an alert on idempotency_keys row count. Total count grows with usage; that is normal.
- ❌ NOT firing on partitioned-old rows (REV-FINAL-M6 ships month partitions with 90-day retention). The metric must measure age of **reserved** state, not creation timestamp of any row.

**3. WHY THIS EXISTS**
- Master plan §4 WS-10: `idempotency_keys_reserved_age > 5min`.
- WS-1 redesign per REV-FINAL-M2 commits idem rows synchronously inside the wallet tx. If the metric is non-zero for more than 5 minutes, the tx is hung or an outbox processor is stuck.

**4. DO**
1. Confirm with Track A1 (WS-1 owner) that the WS-1 implementation exposes `idempotency_keys_oldest_reserved_age_seconds` as a gauge. If not, request its emission as part of T1.
2. Write the alert.

**5. DON'T**
- ❌ Don't compute this in PromQL from `idempotency_keys_inserted_total` rate; that doesn't carry age information.
- ❌ Don't use a histogram bucket; a gauge is more direct.

**6. HOW (concrete)**
```yaml
groups:
- name: ws10-idem
  rules:
  - alert: IdempotencyKeyReservedTooLong
    expr: max(idempotency_keys_oldest_reserved_age_seconds) > 300
    for: 5m
    labels:
      severity: critical
      ws: ws-01
    annotations:
      summary: "Idempotency key reserved > 5 minutes (oldest = {{$value}}s)"
      runbook: "https://runbooks.internal/alm/ws10-idem-stuck"
```

**7. VERIFY**
```bash
promtool check rules deploy/observability/alerts/ws10-idem.yaml | tee logs/WS10-T18-verify.txt
```

**8. DEPENDENCIES / OWNER**
- Blocked by: WS-1 T1 metric emission.
- Owner: Track G + Track A1.

---

### T19 — Alert rule: `gw_routing_*_size` high-water (10m)

**1. WHAT IT IS**
- Alert when any routing map exceeds 80% of its configured cap for 10 minutes (WS-3 routing caps default 10k per master plan §4 WS-3).

**2. WHAT IT IS NOT**
- ❌ NOT alerting on size > 0 (steady-state nonzero is normal).
- ❌ NOT alerting on eviction rate (that has its own alert in T13's dashboard via burn-rate).

**3. WHY THIS EXISTS**
- Master plan §4 WS-10: `gw_routing_*_size 高水位`. If a routing map approaches its cap, requests are about to be evicted and routing decisions start drifting.

**4. DO**
1. Read WS-3 routing implementation to obtain the canonical cap config key (likely `routing.<name>.cap`). Use a recording rule to compute % full per map.
2. Author the alert.

**5. DON'T**
- ❌ Don't hardcode 10000 — read from the per-map cap if exposed; otherwise alert at 8000 with a runbook note.

**6. HOW (concrete)**
```yaml
groups:
- name: ws10-routing
  rules:
  - alert: RoutingMapHighWaterMark
    expr: max by (name) (gw_routing_size{name=~".+"}) / max by (name) (gw_routing_cap{name=~".+"}) > 0.8
    for: 10m
    labels:
      severity: warning
      ws: ws-03
    annotations:
      summary: "Routing map {{$labels.name}} > 80% of cap"
      runbook: "https://runbooks.internal/alm/ws10-routing-cap"
```

**7. VERIFY**
```bash
promtool check rules deploy/observability/alerts/ws10-routing.yaml | tee logs/WS10-T19-verify.txt
```

**8. DEPENDENCIES / OWNER**
- Blocked by: WS-3 routing cap implementation exposing `gw_routing_size` / `gw_routing_cap`.
- Owner: Track G + Track D.

---

### T20 — Alert rule: `gw_tunnel_active_streams` monotonic growth (15m)

**1. WHAT IT IS**
- Alert when `gw_tunnel_active_streams` (per node) grows monotonically over a 15-minute window — leak indicator.

**2. WHAT IT IS NOT**
- ❌ NOT alerting on absolute count (legitimate peaks during load).
- ❌ NOT alerting on transient blips (the `for: 15m` ensures sustained growth).

**3. WHY THIS EXISTS**
- Master plan §4 WS-10: `gw_tunnel_active_streams 持续涨`. WS-2 redesign (REV-FINAL-W8 close.await + scopeguard) eliminates the leak in theory; this alert is the production canary.

**4. DO**
1. Use `deriv()` over the window; if positive consistently, fire.

**5. DON'T**
- ❌ Don't aggregate across nodes — per-node leak detection is the point.

**6. HOW (concrete)**
```yaml
groups:
- name: ws10-tunnel
  rules:
  - alert: TunnelActiveStreamsLeak
    expr: deriv(gw_tunnel_active_streams[15m]) > 0
    for: 15m
    labels:
      severity: warning
      ws: ws-02
    annotations:
      summary: "Tunnel node {{$labels.node}} stream count growing for 15m (rate = {{$value}})"
      runbook: "https://runbooks.internal/alm/ws10-tunnel-leak"
```

**7. VERIFY**
```bash
promtool check rules deploy/observability/alerts/ws10-tunnel.yaml | tee logs/WS10-T20-verify.txt
```

**8. DEPENDENCIES / OWNER**
- Blocked by: WS-2 tunnel stream count metric.
- Owner: Track G + Track D.

---

### T21 — Runbook: per P0 invariant entry + per new alert

**1. WHAT IT IS**
- Create runbook entries in `docs/runbooks/` (Go side) and `ailinkmax-gateway/docs/runbooks/` (Rust side), one per:
  - P0 invariant from WS-1..WS-9 (master plan §2 lists each P0).
  - Each alert created in T17-T20.
- Each runbook entry uses the same 8-section format (renamed for ops): IS / IS NOT / WHY (what broke) / CHECK (commands to triage) / MITIGATE (immediate stop-bleed) / FIX (root-cause path) / VERIFY (resolution evidence) / OWNER.

**2. WHAT IT IS NOT**
- ❌ NOT a tutorial. Targeted at on-call SRE who must triage in <5 min.
- ❌ NOT a place to debate design. Cite master plan WS / task ID.

**3. WHY THIS EXISTS**
- Master plan §7 #13 (REV-006): "runbook 完整性：每个 §7 #6 列举的 lint 与新告警必有对应 runbook 条目；缺失即拒".

**4. DO**
1. List P0 invariants by reading master plan §2.1-2.4.
2. For each, write a runbook entry.
3. For each T17-T20 alert, write a runbook entry under the URL referenced in the alert annotation.
4. Add a README index `docs/runbooks/INDEX.md` listing entries by alert / WS.

**5. DON'T**
- ❌ Don't auto-link runbooks to PagerDuty severities here — PagerDuty config is out of scope for this PR. Just include the alert label `severity:`.
- ❌ Don't include credentials or DSNs in runbook commands. Reference env vars.

**6. HOW (concrete)** — see template at the bottom of this section.

**7. VERIFY**
```bash
ls docs/runbooks/ws10-*.md | wc -l                     # >= 4 (T17-T20) + per-P0
ls ailinkmax-gateway/docs/runbooks/ws10-*.md           # mirror
grep -L 'OWNER' docs/runbooks/ws10-*.md                # empty (all have OWNER)
```
Log to `logs/WS10-T21-verify.txt`.

**8. DEPENDENCIES / OWNER**
- Blocks: master plan §7 #13 acceptance.
- Blocked by: T17, T18, T19, T20.
- Owner: Track G.

**Runbook entry template (`docs/runbooks/ws10-<slug>.md`):**
```markdown
# Runbook: <Alert / Invariant Name>

**IS**: One-sentence description (what fires this).
**IS NOT**: Common false-positive triggers.
**WHY**: Root cause this alert protects against (cite master plan WS-X task).
**CHECK**:
  - `kubectl -n alm logs deploy/<binary> | tail -200`
  - `curl -s localhost:9090/metrics | grep <metric>`
  - PromQL: `<query>`
**MITIGATE**: Immediate stop-bleed steps (e.g., scale up replicas, drain node).
**FIX**: Permanent fix path (link to code, master plan task).
**VERIFY**: How to confirm resolved (specific metric returning to baseline).
**OWNER**: <team / slack channel>
```

---

### T22 — License H7 14-day warn-only enforcement + feature flag

**1. WHAT IT IS**
- Implement the License middleware (`modules/license/api/middleware.go` — file path subject to verification via Read) with three modes per REV-004e:
  - `disabled` — no checks (today's behaviour).
  - `warn` — log and metric on violation, allow request.
  - `enforce` — return 402 with `X-Reason: license_required` per master plan §4 WS-5 S5.1.
- Mode controlled by `[license] enforce: enum{disabled, warn, enforce} = warn` in config (default `warn`).
- Add `flags/license_enforce.yaml` per master plan §4 WS-15.
- Schedule: deploy as `warn` mode → 14 days production observation → flip to `enforce` only after WS-16 customer comms complete (T23).

**2. WHAT IT IS NOT**
- ❌ NOT an immediate 402 rollout. REV-FINAL-LICENSE-COMM explicitly forbids cutting customers off without 14-day warn window + comms.
- ❌ NOT a redesign of license bootstrap. `e2e_bootstrap.go` hard-block per master plan S5.2 stays.
- ❌ NOT a deletion of `ALM_E2E_LICENSE_BOOTSTRAP` — H8 owns that (master plan §4 WS-5 S5.2).

**3. WHY THIS EXISTS**
- Original audit H7: license check exists but middleware does not enforce; today every endpoint is reachable without a license.
- REV-004e: enforcing without notice = customer outage.
- REV-FINAL-LICENSE-COMM (v2 §4 WS-10): 14-day warn-only + coordinated comms.

**4. DO**
1. Implement the middleware reading config-driven `enforce` mode.
2. Emit metric `license_block_total{reason, mode}` always (even in `warn` mode).
3. Write `flags/license_enforce.yaml` documenting the rollout plan.
4. Add a feature-flag table-test in `modules/license/api/middleware_test.go` covering all three modes × {licensed, unlicensed} matrix.
5. Default config: `warn`. Production secret/override must be set by SRE during cutover.

**5. DON'T**
- ❌ Don't gate `/livez`, `/readyz`, `/api/license/*`, `/api/auth/login`, `/api/public/*` (REV-FINAL-V13 whitelist).
- ❌ Don't write the license decision to the database on every request. Use the in-memory cached license state.
- ❌ Don't strip the `X-Reason` header in `warn` mode — operators want to see what would have been blocked.

**6. HOW (concrete)**
- Read `modules/license/business/` to find the existing `Status() (state, error)` API. Reuse it.
- Middleware skeleton (file `modules/license/api/middleware.go`):
  ```go
  func Middleware(svc Service, mode Mode) func(http.Handler) http.Handler {
      return func(next http.Handler) http.Handler {
          return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
              if isWhitelisted(r.URL.Path) {
                  next.ServeHTTP(w, r)
                  return
              }
              st := svc.Status()
              if st.Licensed {
                  next.ServeHTTP(w, r)
                  return
              }
              metrics.Counter("license_block_total", map[string]string{
                  "reason": st.Reason, "mode": mode.String(),
              }).Inc()
              switch mode {
              case ModeDisabled, ModeWarn:
                  w.Header().Set("X-License-Warning", "license_required")
                  next.ServeHTTP(w, r)
              case ModeEnforce:
                  w.Header().Set("X-Reason", "license_required")
                  w.WriteHeader(http.StatusPaymentRequired)
              }
          })
      }
  }
  ```
- Wire into `BuildRouter` per REV-FINAL-V13 order: `RequestID → panicRecover (Observe) → license → auth → authz → handler`. The current `host.go:36-43` order needs the `license` middleware inserted after `Observe`.

**7. VERIFY**
1. Unit: `go test -race ./modules/license/...` covers the 3×2 matrix; assertions on response code + `X-Reason` / `X-License-Warning` header + `license_block_total` increment.
2. e2e: with `[license] enforce = warn`, unlicensed install still serves traffic; metric `license_block_total{mode="warn"}` increments.
3. e2e: with `[license] enforce = enforce`, unlicensed → 402.
4. Log to `logs/WS10-T22-verify.txt`.

**8. DEPENDENCIES / OWNER**
- Blocks: T23 (comms references the rollout plan).
- Blocked by: T03 (readyz must work because license middleware sits after Observe and before route handlers; readyz is whitelisted so it doesn't depend on license, but the chain must be sane).
- Owner: Track G (SRE) + Track F1 (Senior Sec #1, license is auth-adjacent).

---

### T23 — License H7 customer communication coordination

**1. WHAT IT IS**
- Coordination ticket and email template for the 14-day warn → enforce transition.
- Deliverables (committed to repo, not just sent):
  - `docs/runbooks/license-enforcement-rollout.md` — the SRE playbook.
  - `docs/comms/2026-06-XX-license-enforcement.md` — the customer-facing notice (Markdown + plain-text variant).
  - PagerDuty / on-call rotation schedule for the 14-day window.

**2. WHAT IT IS NOT**
- ❌ NOT actually sending emails — that is a manual step by the comms owner. T23 ships the templates.
- ❌ NOT a legal review (handled separately).

**3. WHY THIS EXISTS**
- REV-FINAL-LICENSE-COMM + WS-15 + WS-16: customer notification is required before flipping `enforce`.

**4. DO**
1. Write the rollout runbook with day-by-day checklist (Day 0 ship warn-only, Day 7 first reminder email, Day 12 second reminder, Day 14 flip enforce).
2. Write the customer-facing notice (concise: what is changing, what the customer needs to do, what happens if they don't).
3. Identify owners: comms team, support team, SRE on-call rotation.
4. Add an INVESTIGATE entry if any customer's contract status is unclear.

**5. DON'T**
- ❌ Don't write the notice in a tone that implies the customer did something wrong — this is a product-side rollout.
- ❌ Don't promise compensation or extensions in the template — that's a sales-team decision.

**6. HOW (concrete)**
- `docs/runbooks/license-enforcement-rollout.md` table:
  | Day | Action | Owner | Verification |
  |---|---|---|---|
  | D0 | Deploy `warn` mode | SRE on-call | `license_block_total{mode="warn"}` visible |
  | D0+1 | First customer notice email batch | Comms | List of affected customers + sent timestamps |
  | D7 | Second reminder | Comms | Email metrics |
  | D12 | Final reminder | Comms | Email metrics |
  | D14 | Flip to `enforce` in staging | SRE | metric switch |
  | D14+24h | Flip to `enforce` in prod | SRE | metric switch |
  | D15 | Postmortem of unexpected 402 rate | SRE + Support | Drill rate |
- Template message stub:
  ```markdown
  Subject: Action required: your AILinkMax license check will be enforced on YYYY-MM-DD

  Hi {{customer_name}},

  As part of our upcoming 1.0 release, AILinkMax will begin enforcing the license check that has been running in warning-only mode since YYYY-MM-DD. After {{enforce_date}}, requests on instances without a valid license will receive `HTTP 402 Payment Required`.

  Your instance(s) currently have status: {{license_state}}.

  To avoid disruption, please {{required_action}} by {{deadline}}.

  Questions? Reply to this email or contact support@{{domain}}.
  ```

**7. VERIFY**
```bash
test -f docs/runbooks/license-enforcement-rollout.md
test -f docs/comms/2026-06-17-license-enforcement.md
echo "OK" | tee logs/WS10-T23-verify.txt
```

**8. DEPENDENCIES / OWNER**
- Blocks: master plan §7 #14 (P0 security disclosure / customer-impacting changes acceptance).
- Blocked by: T22.
- Owner: Track G (SRE) coordinating with WS-15 (Flag rollout) and WS-16 (Security/Comms).

---

### T24 — INVESTIGATE-001 close-out (SH1-3)

**1. WHAT IT IS**
- Resolve v2 §99 INVESTIGATE-001: identify what "SH1-3" referred to in v1 §2.1 and §3 WS-10.
- Output: `docs/issues/2026-06-17-investigates/INVESTIGATE-001.md` with either:
  - (a) the concrete defects SH1-3 represented, folded into WS-10 task list with new T-ids, or
  - (b) a refutation moving SH1-3 to v1 §2.5 已证伪.

**2. WHAT IT IS NOT**
- ❌ NOT a code change. This is an investigation ticket.

**3. WHY THIS EXISTS**
- v2 REV-002: SH1-3 is an orphan identifier referenced but never defined. P4 forbids floating identifiers.

**4. DO**
1. Send a SendMessage to sonnet agent `a3aa9c0a6443d4772` (master plan appendix A or v2 §1.2 cites it).
2. Capture the response to `docs/issues/2026-06-17-investigates/INVESTIGATE-001.md`.
3. If concrete defects emerge, file a follow-up PR adding them as new WS-10 tasks.

**5. DON'T**
- ❌ Don't ship WS-10 with SH1-3 still floating.

**6. HOW (concrete)**
- Use the master agent's SendMessage tool. Capture text response into the .md file.

**7. VERIFY**
```bash
test -f docs/issues/2026-06-17-investigates/INVESTIGATE-001.md
grep -E 'RESOLUTION|REFUTED|FOLDED-INTO' docs/issues/2026-06-17-investigates/INVESTIGATE-001.md | tee logs/WS10-T24-verify.txt
```

**8. DEPENDENCIES / OWNER**
- Blocks: master plan v2 §99 closure.
- Owner: Track G.

---

## 4. WS-10 DoD (Done-of-Definition)

Adopted verbatim from master plan §4 WS-10 + REV-005 specifics:

1. **Bad-config startup test**: deploy with intentionally wrong PG DSN; binary exits non-zero within 6s; `kubectl describe pod` shows the `slog.Error` line.
2. **Log redaction sample**: random sample of 10,000 log lines from a 1h soak run; zero matches against any pattern in `SensitivePatterns()`. Tool: `scripts/check-log-sample.sh` (one-shot, not CI).
3. **Prom scrape size**: `/metrics` payload at peak load is < 5 MB (master plan v1 §4 WS-3 DoD, adopted by WS-10 because R15 ownership is shared).
4. **Grafana dashboards render**: every panel in `ws01-ws10` dashboards shows data during `make e2e-soak-smoke`.
5. **Alert rules pass `promtool`**: all `.yaml` in `deploy/observability/alerts/` pass `promtool check rules`.
6. **Runbook coverage**: every alert in the alerts directory has a runbook file referenced by its `runbook:` annotation; every P0 invariant has an entry.
7. **License rollout artifacts**: `docs/runbooks/license-enforcement-rollout.md` + `docs/comms/2026-06-17-license-enforcement.md` exist and pass review.
8. **CI lints**: `scripts/check-log-redaction.sh` and `scripts/check-metric-labels.sh` pass cleanly on `make check`.
9. **INVESTIGATE-001**: closed (either folded or refuted).
10. **Mirror**: `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-10-ops-hardening.md` is byte-identical to this file modulo the H1 title (CLAUDE.md sync rule).

---

## 5. Cross-references

- Master plan v1: `docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-10, §5.
- Master plan v2: `docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §0 重要 1+2, §4 WS-10 (REV-FINAL-R15-obs, REV-FINAL-LICENSE-COMM).
- Original audits: S1, S7 (Go), R15 (Rust).
- Sibling sub-plans (in progress): WS-2, WS-3, WS-4, WS-5 per current task list.
- Source-verified line numbers (2026-06-17, plan author):
  - `AILinkMax-Go/shared/platform/store/pg.go:21-45` — no Ping in NewPG.
  - `AILinkMax-Go/shared/platform/store/redis.go:6-12` — no Ping in NewRedis.
  - `AILinkMax-Go/shared/platform/store/clickhouse.go:44-46` — no Ping in NewClickHouse.
  - `AILinkMax-Go/shared/runtime/host.go:45-46` — `/livez` and `/readyz` are trivial 200 OK.
  - `AILinkMax-Go/shared/runtime/httplog.go:57-63, 98-103` — `r.URL.Path`, `r.RemoteAddr`, `r.UserAgent()` logged without redaction.
  - `ailinkmax-gateway/crates/gw-ledger/src/inflight_reaper.rs:119-124` — `request_id` Prom label (kill target).
  - `ailinkmax-gateway/crates/gw-ledger/src/detailed_archive.rs:222` — false positive (Redis XADD field, not Prom label).

— end WS-10 sub-plan —
