# Chaos Soak Test System — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a 50,000-req / 60-s soak harness that drives the real gateway under mixed protocols + configurable upstream chaos, producing AI-consumable JSON+JSONL reports with all hard invariants verified, per `docs/superpowers/specs/2026-05-22-chaos-soak-design.md`.

**Architecture:** Three-end split with **zero gateway code changes** (C1). Rust extensions to `e2e-fake-upstream` (weighted DSL + chaos flag + hit counters) and `e2e-fake-web` (soak-window aggregation + drain-quiescence). Python `soak/` module under `tests/e2e/client/` provides the async driver, invariants, and JSON tree report. Orchestrator is a single pytest file with three test functions (smoke / chaos-off-60s / chaos-heavy-60s). All numeric values pinned per spec §11 (C6).

**Tech Stack:** Rust 1.86 (workspace pinned), `rand 0.8` `WeightedIndex`, `axum` for fake-web admin, Python `asyncio + httpx.AsyncClient` driven from `concurrent.futures.ThreadPoolExecutor`, `pytest` markers, `docker compose` (existing e2e harness).

**Source of truth:** `docs/superpowers/specs/2026-05-22-chaos-soak-design.md`. When in doubt during implementation, the spec wins.

---

## Phase 1 — fake-upstream Rust extensions

### Task 1: `ResponseSpec::Weighted` DSL variant + validation

**Files:**
- Modify: `crates/e2e-fake-upstream/src/scenario.rs` (add variant, struct, validator)
- Modify: `crates/e2e-fake-upstream/Cargo.toml` (already has `rand = "0.8"` indirectly; add direct dep)
- Test: `crates/e2e-fake-upstream/tests/weighted.rs` (new file)

Per spec §4.1, add a `Weighted { variants }` variant to `ResponseSpec`. Empty `variants` and nested `Weighted` MUST be rejected at deserialize time so admin posts fail loudly, not at dispatch time.

- [ ] **Step 1: Add `rand` to `e2e-fake-upstream` Cargo.toml**

Edit `crates/e2e-fake-upstream/Cargo.toml`, add under `[dependencies]`:

```toml
rand = "0.8"
```

- [ ] **Step 2: Write the failing test for Weighted deserialization**

Create `crates/e2e-fake-upstream/tests/weighted.rs`:

```rust
//! Unit tests for the `ResponseSpec::Weighted` DSL variant (E2E chaos soak).

use e2e_fake_upstream::scenario::{validate_scenario, ResponseSpec, Scenario};

const HAPPY_WEIGHTED_JSON: &str = r#"{
    "routes": [{
        "match": {"method": "POST", "path_regex": "^/v1/chat/completions$"},
        "response": {
            "kind": "weighted",
            "variants": [
                {"weight": 90, "spec": {"kind": "sse", "delay_ms": 0, "events": [{"data": "hi"}]}},
                {"weight": 10, "spec": {"kind": "tcp_reset_mid_body", "after_bytes": 256}}
            ]
        }
    }]
}"#;

#[test]
fn weighted_variant_round_trips() {
    let s: Scenario = serde_json::from_str(HAPPY_WEIGHTED_JSON).unwrap();
    assert_eq!(s.routes.len(), 1);
    match &s.routes[0].response {
        ResponseSpec::Weighted { variants } => {
            assert_eq!(variants.len(), 2);
            assert_eq!(variants[0].weight, 90);
            assert_eq!(variants[1].weight, 10);
        }
        other => panic!("expected Weighted, got {other:?}"),
    }
}

#[test]
fn validate_rejects_empty_variants() {
    let raw = r#"{
        "routes": [{
            "match": {"method": "POST", "path_regex": "^/x$"},
            "response": {"kind": "weighted", "variants": []}
        }]
    }"#;
    let s: Scenario = serde_json::from_str(raw).unwrap();
    let err = validate_scenario(&s).expect_err("empty variants must be rejected");
    assert!(err.contains("empty variants"), "got: {err}");
}

#[test]
fn validate_rejects_nested_weighted() {
    let raw = r#"{
        "routes": [{
            "match": {"method": "POST", "path_regex": "^/x$"},
            "response": {
                "kind": "weighted",
                "variants": [
                    {"weight": 1, "spec": {
                        "kind": "weighted",
                        "variants": [
                            {"weight": 1, "spec": {"kind": "sse", "delay_ms": 0, "events": []}}
                        ]
                    }}
                ]
            }
        }]
    }"#;
    let s: Scenario = serde_json::from_str(raw).unwrap();
    let err = validate_scenario(&s).expect_err("nested weighted must be rejected");
    assert!(err.contains("nested"), "got: {err}");
}
```

Also derive `Debug` on `ResponseSpec` and `WeightedVariant` (the assert above prints it on failure). If `ResponseSpec` does not already derive `Debug`, add it.

- [ ] **Step 3: Run the test to confirm it fails**

```bash
cargo test -p e2e-fake-upstream --test weighted
```

Expected: build error — `Weighted` not a variant of `ResponseSpec`, `WeightedVariant` not a type, `validate_scenario` not a function.

- [ ] **Step 4: Add the `Weighted` variant + `WeightedVariant` struct**

In `crates/e2e-fake-upstream/src/scenario.rs`, derive `Debug` on `ResponseSpec` (and add `Debug` to all existing variants if missing). Inside the `ResponseSpec` enum, append:

```rust
    /// Weighted-random dispatch (E2E chaos soak, §4.1 of design spec).
    ///
    /// `variants[0]` is required to be the happy-path: when `CHAOS_ENABLED`
    /// is off the dispatch falls through to `variants[0]` unconditionally.
    /// Empty `variants` and nested `Weighted` are rejected by
    /// [`validate_scenario`] before reaching dispatch.
    Weighted {
        variants: Vec<WeightedVariant>,
    },
```

After the `ResponseSpec` enum, add:

```rust
#[derive(serde::Deserialize, Clone, Debug)]
pub struct WeightedVariant {
    pub weight: u32,
    pub spec: Box<ResponseSpec>,
}
```

- [ ] **Step 5: Add the `validate_scenario` function**

At the bottom of `crates/e2e-fake-upstream/src/scenario.rs`, append:

```rust
/// Validate a deserialised [`Scenario`] before publishing it as the active
/// scenario. Rejects shapes that would produce undefined behaviour at
/// dispatch time:
///
/// - Empty `Weighted.variants` (no candidate to pick).
/// - Nested `Weighted` (dispatch unwraps exactly one layer; §4.3 of design).
///
/// Returns `Err(msg)` with a human-readable reason; caller maps to HTTP 400.
pub fn validate_scenario(s: &Scenario) -> Result<(), String> {
    for (i, route) in s.routes.iter().enumerate() {
        validate_response(&route.response, i, 0)?;
    }
    Ok(())
}

fn validate_response(r: &ResponseSpec, route_idx: usize, depth: u32) -> Result<(), String> {
    if let ResponseSpec::Weighted { variants } = r {
        if depth >= 1 {
            return Err(format!(
                "route[{route_idx}]: nested Weighted is not allowed \
                 (dispatch unwraps exactly one layer)"
            ));
        }
        if variants.is_empty() {
            return Err(format!(
                "route[{route_idx}]: Weighted with empty variants \
                 (dispatch has no candidate)"
            ));
        }
        for (vi, v) in variants.iter().enumerate() {
            validate_response(&v.spec, route_idx, depth + 1).map_err(|e| {
                format!("route[{route_idx}].variants[{vi}]: {e}")
            })?;
        }
    }
    Ok(())
}
```

- [ ] **Step 6: Run the test to confirm it passes**

```bash
cargo test -p e2e-fake-upstream --test weighted
```

Expected: all 3 tests PASS.

- [ ] **Step 7: Format + clippy**

```bash
cargo fmt -p e2e-fake-upstream
cargo clippy -p e2e-fake-upstream --all-targets -- -D warnings
```

Expected: zero output.

- [ ] **Step 8: Commit**

```bash
git add crates/e2e-fake-upstream/Cargo.toml crates/e2e-fake-upstream/src/scenario.rs crates/e2e-fake-upstream/tests/weighted.rs
git commit -m "soak(fake-upstream): ResponseSpec::Weighted variant + validator

Adds weighted-random dispatch variant for chaos soak. Validator rejects
empty variants and nested Weighted at admin/scenario deserialize time so
the dispatch path can assume one-layer unwrap (design §4.1, §4.3)."
```

---

### Task 2: `CHAOS_ENABLED` env flag + `dispatch_spec` unwrap layer

**Files:**
- Modify: `crates/e2e-fake-upstream/src/main.rs` (read `FAKE_UPSTREAM_CHAOS` at boot)
- Create: `crates/e2e-fake-upstream/src/chaos.rs` (new module: flag + weighted pick)
- Modify: `crates/e2e-fake-upstream/src/lib.rs` (export `chaos`)
- Modify: `crates/e2e-fake-upstream/src/server.rs` (`dispatch_spec` unwrap)
- Test: append to `crates/e2e-fake-upstream/tests/weighted.rs`

Per spec §4.2, the flag is boot-time-only — orchestrator must restart the container to flip it. Per §4.3 the dispatch_spec unwraps exactly one `Weighted` layer.

- [ ] **Step 1: Write the failing test for chaos-off fall-through**

Append to `crates/e2e-fake-upstream/tests/weighted.rs`:

```rust
use e2e_fake_upstream::chaos::{set_enabled_for_test, ChaosState};
use e2e_fake_upstream::scenario::{MatchSpec, Route, SseEvent, WeightedVariant};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

fn weighted_route() -> Route {
    Route {
        r#match: MatchSpec {
            method: Some("POST".into()),
            path_regex: Some("^/x$".into()),
            header_eq: None,
        },
        auth_check: None,
        response: ResponseSpec::Weighted {
            variants: vec![
                WeightedVariant {
                    weight: 1,
                    spec: Box::new(ResponseSpec::Sse {
                        delay_ms: 0,
                        events: vec![SseEvent { data: "HAPPY".into(), after_ms: 0 }],
                    }),
                },
                WeightedVariant {
                    weight: 1,
                    spec: Box::new(ResponseSpec::Static {
                        status: 500,
                        headers: Default::default(),
                        body: "FAULT".into(),
                        delay_ms: 0,
                    }),
                },
            ],
        },
    }
}

#[tokio::test(flavor = "current_thread")]
async fn chaos_off_always_picks_first_variant() {
    use e2e_fake_upstream::routes::make_counters;
    use e2e_fake_upstream::server::serve_business;
    use e2e_fake_upstream::inject::RefuseCounter;

    set_enabled_for_test(false);

    let scenario = Scenario { routes: vec![weighted_route()] };
    let counters = make_counters(&scenario);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let s = Arc::new(RwLock::new(scenario));
    let c = Arc::new(RwLock::new(counters));
    tokio::spawn(serve_business(listener, s, c, RefuseCounter::new()));

    // 5 sequential requests — chaos OFF, variant[0] (HAPPY sse) must win every time.
    for _ in 0..5 {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(
            b"POST /x HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let body = String::from_utf8_lossy(&buf);
        assert!(body.contains("HAPPY"), "chaos=off must serve variant[0]; got: {body}");
        assert!(!body.contains("FAULT"), "chaos=off leaked variant[1]: {body}");
    }
}
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cargo test -p e2e-fake-upstream --test weighted chaos_off
```

Expected: build error — `chaos` module and `set_enabled_for_test` do not exist; `dispatch_spec` has no `Weighted` arm.

- [ ] **Step 3: Create the chaos module**

Create `crates/e2e-fake-upstream/src/chaos.rs`:

```rust
//! Chaos-mode flag + weighted-pick helper (E2E soak, §4.2/§4.3).
//!
//! Flag is **boot-time only** — orchestrator restarts the container to flip
//! it. `set_enabled_for_test` exists so unit tests can drive both modes
//! within one process; production paths only read via [`enabled`].

use std::sync::atomic::{AtomicBool, Ordering};

use rand::distributions::{Distribution, WeightedIndex};
use rand::thread_rng;

use crate::scenario::{ResponseSpec, WeightedVariant};

static CHAOS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Read `FAKE_UPSTREAM_CHAOS` from the process env and store into the global.
/// Recognised truthy values: `on`, `1`, `true` (case-sensitive — keep it
/// boring; docker compose env passthrough is exact).
pub fn init_from_env() {
    let on = std::env::var("FAKE_UPSTREAM_CHAOS")
        .map(|s| matches!(s.as_str(), "on" | "1" | "true"))
        .unwrap_or(false);
    CHAOS_ENABLED.store(on, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    CHAOS_ENABLED.load(Ordering::Relaxed)
}

/// Test-only setter. Production callers MUST use [`init_from_env`].
#[doc(hidden)]
pub fn set_enabled_for_test(on: bool) {
    CHAOS_ENABLED.store(on, Ordering::SeqCst);
}

/// Pick a variant by weight. Caller must guarantee `variants` is non-empty
/// (validator rejects empty at admin/scenario time).
pub fn pick_weighted(variants: &[WeightedVariant]) -> ResponseSpec {
    debug_assert!(!variants.is_empty(), "validator must have rejected empty variants");
    if variants.len() == 1 {
        return (*variants[0].spec).clone();
    }
    let weights = variants.iter().map(|v| v.weight.max(1));
    let dist = WeightedIndex::new(weights).expect("all weights >= 1 after max");
    let idx = dist.sample(&mut thread_rng());
    (*variants[idx].spec).clone()
}
```

- [ ] **Step 4: Export `chaos` from the library**

In `crates/e2e-fake-upstream/src/lib.rs`, after the existing module declarations:

```rust
pub mod chaos;
```

- [ ] **Step 5: Wire `dispatch_spec` to unwrap one `Weighted` layer**

In `crates/e2e-fake-upstream/src/server.rs`, replace the body of `dispatch_spec` so the FIRST line resolves `Weighted` once, then the existing `match` proceeds on the resolved spec:

```rust
async fn dispatch_spec(stream: TcpStream, spec: ResponseSpec) -> std::io::Result<()> {
    // §4.3: unwrap exactly one Weighted layer. Validator rejects nested
    // Weighted and empty variants, so this branch is total.
    let spec = match spec {
        ResponseSpec::Weighted { variants } => {
            if crate::chaos::enabled() {
                crate::chaos::pick_weighted(&variants)
            } else {
                (*variants[0].spec).clone()
            }
        }
        other => other,
    };
    match spec {
        ResponseSpec::Static { status, headers, body, delay_ms } => {
            inject::write_static(stream, status, headers, body, delay_ms).await
        }
        ResponseSpec::Sse { delay_ms, events } => inject::write_sse(stream, delay_ms, events).await,
        ResponseSpec::TcpResetAfterHeaders => inject::tcp_reset_after_headers(stream).await,
        ResponseSpec::TcpResetMidBody { after_bytes } => {
            inject::tcp_reset_mid_body(stream, after_bytes).await
        }
        ResponseSpec::ConnectionRefusedFirstN { .. } => {
            inject::write_static(
                stream,
                200,
                std::collections::HashMap::from([(
                    "content-type".into(),
                    "application/json".into(),
                )]),
                r#"{"ok":true,"note":"refuse counter exhausted"}"#.to_owned(),
                0,
            ).await
        }
        ResponseSpec::SlowBodyBytePerSecond { body, bps } => {
            inject::slow_body_byte_per_second(stream, body, bps).await
        }
        ResponseSpec::PartialSseEvent { half_body } => {
            inject::partial_sse_event(stream, half_body).await
        }
        ResponseSpec::StatusWithRetryAfter { status, retry_after_secs, body } => {
            inject::status_with_retry_after(stream, status, retry_after_secs, body).await
        }
        ResponseSpec::GzipStatic { status, headers, body_base64, delay_ms } => {
            inject::write_gzip_static(stream, status, headers, body_base64, delay_ms).await
        }
        ResponseSpec::Weighted { .. } => {
            // Validator forbids nested Weighted (§4.3); reaching this arm
            // is a bug. Log loudly and close the connection so the test
            // notices instead of silently hanging.
            eprintln!("[fake-upstream] BUG: nested Weighted slipped past validator");
            Ok(())
        }
    }
}
```

- [ ] **Step 6: Wire `init_from_env` at boot**

In `crates/e2e-fake-upstream/src/main.rs`, inside `#[tokio::main] async fn main()`, immediately after the existing `let mode = ...` line, insert:

```rust
    e2e_fake_upstream::chaos::init_from_env();
    if e2e_fake_upstream::chaos::enabled() {
        eprintln!("[fake-upstream] chaos mode ENABLED (FAKE_UPSTREAM_CHAOS=on)");
    }
```

- [ ] **Step 7: Run all weighted tests**

```bash
cargo test -p e2e-fake-upstream --test weighted
```

Expected: 4 tests PASS (3 from Task 1 + 1 new chaos_off test).

- [ ] **Step 8: Format + clippy + commit**

```bash
cargo fmt -p e2e-fake-upstream
cargo clippy -p e2e-fake-upstream --all-targets -- -D warnings
git add crates/e2e-fake-upstream/src/{chaos.rs,lib.rs,main.rs,server.rs} crates/e2e-fake-upstream/tests/weighted.rs
git commit -m "soak(fake-upstream): chaos flag + dispatch_spec Weighted unwrap

FAKE_UPSTREAM_CHAOS env (boot-time) gates a weighted pick over the
existing fault variants. Chaos-off falls through to variants[0] (the
happy-path convention from spec §4.1). One-layer unwrap; nested
Weighted is rejected upstream by validator."
```

---

### Task 3: Per-variant hit counters + `GET /admin/chaos-stats`

**Files:**
- Modify: `crates/e2e-fake-upstream/src/scenario.rs` (counter struct on Weighted)
- Modify: `crates/e2e-fake-upstream/src/chaos.rs` (increment on pick)
- Modify: `crates/e2e-fake-upstream/src/server.rs` (`/admin/chaos-stats` endpoint)
- Test: `crates/e2e-fake-upstream/tests/hits_admin.rs` (new file)

Per spec §4.4. Counters live alongside the active `Scenario` in `SharedScenario`, reset on scenario swap. Variants without `Weighted` parent contribute nothing.

- [ ] **Step 1: Write the failing test**

Create `crates/e2e-fake-upstream/tests/hits_admin.rs`:

```rust
//! Tests for /admin/hits-total and /admin/chaos-stats endpoints.

use std::sync::Arc;
use std::time::Duration;

use e2e_fake_upstream::chaos::set_enabled_for_test;
use e2e_fake_upstream::inject::RefuseCounter;
use e2e_fake_upstream::routes::make_counters;
use e2e_fake_upstream::scenario::{
    MatchSpec, ResponseSpec, Route, Scenario, SseEvent, WeightedVariant,
};
use e2e_fake_upstream::server::{serve_admin, serve_business};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

async fn drive_once(addr: std::net::SocketAddr, path: &str) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
}

async fn fetch_json(addr: std::net::SocketAddr, path: &str) -> serde_json::Value {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let raw = String::from_utf8_lossy(&buf);
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or_else(|e| panic!("bad json: {e}\nraw:\n{raw}"))
}

#[tokio::test(flavor = "current_thread")]
async fn chaos_stats_counts_variants() {
    set_enabled_for_test(true);

    let scenario = Scenario {
        routes: vec![Route {
            r#match: MatchSpec {
                method: Some("POST".into()),
                path_regex: Some("^/x$".into()),
                header_eq: None,
            },
            auth_check: None,
            response: ResponseSpec::Weighted {
                variants: vec![
                    WeightedVariant {
                        weight: 1,
                        spec: Box::new(ResponseSpec::Sse {
                            delay_ms: 0,
                            events: vec![SseEvent { data: "HAPPY".into(), after_ms: 0 }],
                        }),
                    },
                    WeightedVariant {
                        weight: 1,
                        spec: Box::new(ResponseSpec::Static {
                            status: 500,
                            headers: Default::default(),
                            body: "FAULT".into(),
                            delay_ms: 0,
                        }),
                    },
                ],
            },
        }],
    };

    let counters = make_counters(&scenario);
    let business = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let baddr = business.local_addr().unwrap();
    let aaddr = admin.local_addr().unwrap();
    let s = Arc::new(RwLock::new(scenario));
    let c = Arc::new(RwLock::new(counters));
    let refuse = RefuseCounter::new();
    tokio::spawn(serve_business(business, Arc::clone(&s), Arc::clone(&c), refuse.clone()));
    tokio::spawn(serve_admin(admin, s, c, refuse));

    for _ in 0..50 {
        drive_once(baddr, "/x").await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let stats = fetch_json(aaddr, "/admin/chaos-stats").await;
    assert_eq!(stats["enabled"], serde_json::json!(true));
    let variants = stats["variants"].as_array().expect("variants array");
    assert_eq!(variants.len(), 2);
    let sum: u64 = variants.iter().map(|v| v["hits"].as_u64().unwrap()).sum();
    assert_eq!(sum, 50, "every request must increment exactly one variant");
}
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cargo test -p e2e-fake-upstream --test hits_admin chaos_stats_counts_variants
```

Expected: build/parse error — `/admin/chaos-stats` endpoint not registered yet.

- [ ] **Step 3: Add per-variant counters to the active scenario**

The simplest place is parallel to the existing `HitCounters` (which is per-route). Add `WeightedCounters` keyed by `(route_idx, variant_idx)`.

In `crates/e2e-fake-upstream/src/routes.rs`, after the existing `make_counters`:

```rust
use std::sync::atomic::AtomicU64;

/// Per-(route, weighted-variant) hit counters.
///
/// Parallel to [`HitCounters`] but sized only for routes whose response is
/// `ResponseSpec::Weighted`. Indexed by `(route_idx, variant_idx)`.
/// Reset whenever the scenario is replaced via /admin/scenario.
pub type WeightedHits = Vec<Vec<AtomicU64>>;

pub fn make_weighted_counters(scenario: &crate::scenario::Scenario) -> WeightedHits {
    scenario
        .routes
        .iter()
        .map(|r| match &r.response {
            crate::scenario::ResponseSpec::Weighted { variants } => {
                variants.iter().map(|_| AtomicU64::new(0)).collect()
            }
            _ => Vec::new(),
        })
        .collect()
}
```

- [ ] **Step 4: Plumb `WeightedHits` through `SharedWeightedHits`**

In `crates/e2e-fake-upstream/src/server.rs`, near the existing `SharedScenario` / `SharedCounters` aliases:

```rust
pub type SharedWeightedHits = Arc<RwLock<crate::routes::WeightedHits>>;
```

Update `serve_business` and `serve_admin` signatures to also take `SharedWeightedHits`. Update both `tokio::spawn` blocks in `main.rs` and `tests/inject.rs` `spawn_server` helper to pass it.

In `crates/e2e-fake-upstream/src/main.rs`, after the existing `make_counters` call:

```rust
    let initial_weighted = e2e_fake_upstream::routes::make_weighted_counters(&initial_scenario);
    let weighted_hits = Arc::new(RwLock::new(initial_weighted));
```

…and thread `Arc::clone(&weighted_hits)` into both spawned tasks.

In `crates/e2e-fake-upstream/tests/inject.rs::spawn_server`, mirror the change:

```rust
    let weighted_hits = make_weighted_counters(&scenario);
    let weighted_hits = Arc::new(RwLock::new(weighted_hits));
    let scenario = Arc::new(RwLock::new(scenario));
    let counters = Arc::new(RwLock::new(counters));
    tokio::spawn(async move {
        serve_business(listener, scenario, counters, weighted_hits, refuse).await
    });
```

Add `use e2e_fake_upstream::routes::make_weighted_counters;`.

- [ ] **Step 5: Reset weighted counters on scenario swap**

In `crates/e2e-fake-upstream/src/server.rs`, inside `handle_admin` for `("POST", "/admin/scenario")`, after the existing successful deserialise, replace `new_counters` block with:

```rust
                Ok(new_scenario) => {
                    if let Err(e) = crate::scenario::validate_scenario(&new_scenario) {
                        return Ok(json_response(
                            StatusCode::BAD_REQUEST,
                            &serde_json::json!({"error": e}).to_string(),
                        ));
                    }
                    let new_counters = make_counters(&new_scenario);
                    let new_weighted = crate::routes::make_weighted_counters(&new_scenario);

                    prime_refuse_counter(&new_scenario, &refuse);

                    {
                        let mut sc = scenario.write().await;
                        let mut ctr = counters.write().await;
                        let mut wh = weighted_hits.write().await;
                        *sc = new_scenario;
                        *ctr = new_counters;
                        *wh = new_weighted;
                    }

                    Ok(json_response(StatusCode::OK, r#"{"ok":true}"#))
                }
```

Update the `handle_admin` signature + its `service_fn` closure to accept `weighted_hits: SharedWeightedHits` and capture it.

- [ ] **Step 6: Increment counter on pick**

The `pick_weighted` in `chaos.rs` currently only knows about `&[WeightedVariant]`. Move counter increment into the **caller** (`server.rs::dispatch_spec`) since the caller knows the route index. To avoid plumbing that everywhere, add a new helper that returns both the resolved spec AND the picked variant index:

In `crates/e2e-fake-upstream/src/chaos.rs`, add alongside `pick_weighted`:

```rust
/// Like [`pick_weighted`] but also returns the chosen variant index so the
/// caller can attribute the hit to a per-variant counter.
pub fn pick_weighted_indexed(variants: &[WeightedVariant]) -> (usize, ResponseSpec) {
    debug_assert!(!variants.is_empty(), "validator must have rejected empty variants");
    if variants.len() == 1 {
        return (0, (*variants[0].spec).clone());
    }
    let weights = variants.iter().map(|v| v.weight.max(1));
    let dist = WeightedIndex::new(weights).expect("all weights >= 1 after max");
    let idx = dist.sample(&mut thread_rng());
    (idx, (*variants[idx].spec).clone())
}
```

In `crates/e2e-fake-upstream/src/server.rs`, change `dispatch_spec` to accept route context + weighted_hits and increment on pick:

```rust
async fn dispatch_spec(
    stream: TcpStream,
    spec: ResponseSpec,
    route_idx: usize,
    weighted_hits: SharedWeightedHits,
) -> std::io::Result<()> {
    let spec = match spec {
        ResponseSpec::Weighted { variants } => {
            let (vidx, resolved) = if crate::chaos::enabled() {
                crate::chaos::pick_weighted_indexed(&variants)
            } else {
                (0usize, (*variants[0].spec).clone())
            };
            // Increment the per-(route, variant) counter under read-lock
            // (the inner vec is AtomicU64, fetch_add is wait-free).
            let wh = weighted_hits.read().await;
            if let Some(per_route) = wh.get(route_idx) {
                if let Some(slot) = per_route.get(vidx) {
                    slot.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            resolved
        }
        other => other,
    };
    // ... existing arms unchanged ...
}
```

Update the call site in `handle_business_conn` to pass `route_idx` (the value returned by `match_route`) and `Arc::clone(&weighted_hits)`.

- [ ] **Step 7: Add the `/admin/chaos-stats` endpoint**

In `crates/e2e-fake-upstream/src/server.rs::handle_admin`, after the `("POST", "/admin/scenario")` arm:

```rust
        ("GET", "/admin/chaos-stats") => {
            let sc = scenario.read().await;
            let wh = weighted_hits.read().await;
            let mut variants_out: Vec<serde_json::Value> = Vec::new();
            for (ri, route) in sc.routes.iter().enumerate() {
                if let ResponseSpec::Weighted { variants } = &route.response {
                    for (vi, v) in variants.iter().enumerate() {
                        let hits = wh
                            .get(ri)
                            .and_then(|r| r.get(vi))
                            .map(|a| a.load(std::sync::atomic::Ordering::Relaxed))
                            .unwrap_or(0);
                        variants_out.push(serde_json::json!({
                            "route_index": ri,
                            "variant_index": vi,
                            "kind": variant_kind(&v.spec),
                            "weight": v.weight,
                            "hits": hits,
                        }));
                    }
                }
            }
            let body = serde_json::json!({
                "enabled": crate::chaos::enabled(),
                "variants": variants_out,
            })
            .to_string();
            Ok(json_response(StatusCode::OK, &body))
        }
```

…and a small helper at module bottom:

```rust
fn variant_kind(spec: &ResponseSpec) -> &'static str {
    match spec {
        ResponseSpec::Static { .. } => "static",
        ResponseSpec::Sse { .. } => "sse",
        ResponseSpec::TcpResetAfterHeaders => "tcp_reset_after_headers",
        ResponseSpec::TcpResetMidBody { .. } => "tcp_reset_mid_body",
        ResponseSpec::ConnectionRefusedFirstN { .. } => "connection_refused_first_n",
        ResponseSpec::SlowBodyBytePerSecond { .. } => "slow_body_byte_per_second",
        ResponseSpec::PartialSseEvent { .. } => "partial_sse_event",
        ResponseSpec::StatusWithRetryAfter { .. } => "status_with_retry_after",
        ResponseSpec::GzipStatic { .. } => "gzip_static",
        ResponseSpec::Weighted { .. } => "weighted",
    }
}
```

- [ ] **Step 8: Run the test**

```bash
cargo test -p e2e-fake-upstream --test hits_admin chaos_stats_counts_variants
```

Expected: PASS. The `sum == 50` assertion guarantees every dispatched request incremented exactly one variant counter.

- [ ] **Step 9: Run all fake-upstream tests + clippy**

```bash
cargo test -p e2e-fake-upstream
cargo clippy -p e2e-fake-upstream --all-targets -- -D warnings
```

Expected: all PASS, no warnings. The pre-existing `tests/inject.rs` tests will be affected by signature changes; if any fail, the fix is the `spawn_server` update in Step 4.

- [ ] **Step 10: Commit**

```bash
git add crates/e2e-fake-upstream
git commit -m "soak(fake-upstream): per-variant hit counters + /admin/chaos-stats

WeightedHits parallels HitCounters (sized for Weighted routes only),
reset on scenario swap. dispatch_spec increments the (route_idx,
variant_idx) counter on pick. /admin/chaos-stats dumps the table for
the orchestrator's chaos-causal invariant (design §7.1.5)."
```

---

### Task 4: `GET /admin/hits-total` (routes + refused)

**Files:**
- Modify: `crates/e2e-fake-upstream/src/inject.rs` (promote RefuseCounter hit counter)
- Modify: `crates/e2e-fake-upstream/src/server.rs` (new admin endpoint)
- Modify: `crates/e2e-fake-upstream/tests/hits_admin.rs` (test)

Per spec §4.4, this endpoint is the *authoritative basis* for the §7.1.2 account-consistency invariant (`records + dlq == hits_total`). The sum must include both route hits AND refused-connection hits.

- [ ] **Step 1: Write the failing test**

Append to `crates/e2e-fake-upstream/tests/hits_admin.rs`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn hits_total_sums_routes_and_refused() {
    let scenario = Scenario {
        routes: vec![Route {
            r#match: MatchSpec {
                method: Some("POST".into()),
                path_regex: Some("^/x$".into()),
                header_eq: None,
            },
            auth_check: None,
            response: ResponseSpec::ConnectionRefusedFirstN { n: 3 },
        }],
    };
    let counters = make_counters(&scenario);
    let weighted = e2e_fake_upstream::routes::make_weighted_counters(&scenario);
    let business = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let baddr = business.local_addr().unwrap();
    let aaddr = admin.local_addr().unwrap();
    let s = Arc::new(RwLock::new(scenario));
    let c = Arc::new(RwLock::new(counters));
    let wh = Arc::new(RwLock::new(weighted));
    let refuse = RefuseCounter::new();
    e2e_fake_upstream::server::prime_refuse_counter(&*s.read().await, &refuse);
    tokio::spawn(serve_business(business, Arc::clone(&s), Arc::clone(&c),
                                Arc::clone(&wh), refuse.clone()));
    tokio::spawn(serve_admin(admin, s, c, wh, refuse));

    // 3 refused (counter primed to 3), then 2 served (route hit).
    for _ in 0..5 {
        drive_once(baddr, "/x").await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let v = fetch_json(aaddr, "/admin/hits-total").await;
    assert_eq!(v["refused_hit_sum"], serde_json::json!(3));
    assert_eq!(v["routes_hit_sum"], serde_json::json!(2));
    assert_eq!(v["total"], serde_json::json!(5));
}
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cargo test -p e2e-fake-upstream --test hits_admin hits_total_sums
```

Expected: 404 from the admin server (endpoint not registered).

- [ ] **Step 3: Add refused-hit counter inside `RefuseCounter`**

In `crates/e2e-fake-upstream/src/inject.rs`, change `RefuseCounter` to:

```rust
#[derive(Debug, Default, Clone)]
pub struct RefuseCounter {
    remaining: Arc<AtomicU32>,
    refused_total: Arc<AtomicU64>,
}

impl RefuseCounter {
    pub fn new() -> Self {
        Self {
            remaining: Arc::new(AtomicU32::new(0)),
            refused_total: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn set(&self, n: u32) {
        self.remaining.store(n, Ordering::SeqCst);
    }
    /// Returns true if we should refuse this connection.
    ///
    /// Increments the cumulative `refused_total` on a successful take so the
    /// admin endpoint can report the count even after `remaining` is drained.
    pub fn take_one(&self) -> bool {
        loop {
            let cur = self.remaining.load(Ordering::SeqCst);
            if cur == 0 {
                return false;
            }
            if self
                .remaining
                .compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                self.refused_total.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
    }
    pub fn refused_total(&self) -> u64 {
        self.refused_total.load(Ordering::Relaxed)
    }
}
```

(Add `use std::sync::atomic::AtomicU64;` at the top.)

- [ ] **Step 4: Add the `/admin/hits-total` endpoint**

In `crates/e2e-fake-upstream/src/server.rs::handle_admin`, after the chaos-stats arm:

```rust
        ("GET", "/admin/hits-total") => {
            let ctr = counters.read().await;
            let routes_hit_sum: u64 = ctr.iter().map(|a| a.load(Ordering::Relaxed) as u64).sum();
            let refused = refuse.refused_total();
            let body = serde_json::json!({
                "routes_hit_sum": routes_hit_sum,
                "refused_hit_sum": refused,
                "total": routes_hit_sum + refused,
            })
            .to_string();
            Ok(json_response(StatusCode::OK, &body))
        }
```

(Add `use std::sync::atomic::Ordering;` to `server.rs` if not already imported.)

- [ ] **Step 5: Run all fake-upstream tests + clippy**

```bash
cargo test -p e2e-fake-upstream
cargo clippy -p e2e-fake-upstream --all-targets -- -D warnings
```

Expected: all PASS (5 tests in `weighted.rs` + 2 in `hits_admin.rs` + existing `inject.rs` tests).

- [ ] **Step 6: Commit**

```bash
git add crates/e2e-fake-upstream
git commit -m "soak(fake-upstream): /admin/hits-total = routes + refused

RefuseCounter now also tracks cumulative refused_total (independent of
remaining) so /admin/hits-total can report (routes_hit_sum +
refused_hit_sum). This sum is the authoritative basis for the
account-consistency invariant in spec §7.1.2."
```

---

## Phase 2 — fake-web Rust extensions

### Task 5: `SoakState` + `POST /soak/window/start`

**Files:**
- Create: `crates/e2e-fake-web/src/soak.rs`
- Modify: `crates/e2e-fake-web/src/lib.rs` (mount route, extend `AppState`)
- Test: `crates/e2e-fake-web/tests/soak_window.rs` (new file)

Per spec §5.1. The window's `snapshot_id` is a hex string (no `uuid` dep — workspace doesn't currently include `uuid`). Snapshot captures `XLEN` of the four streams + the `LAST_ID` of `usage:detailed` + per-user balance values.

- [ ] **Step 1: Write the failing test**

Create `crates/e2e-fake-web/tests/soak_window.rs`:

```rust
//! Integration test for /soak/window/start + /soak/window/report.
//!
//! Uses testcontainers to spin Redis + Postgres so the Store::new
//! constructor (which connects to both) does not panic.

#![cfg(feature = "integration")]

// The actual #[tokio::test] body is in Task 7 once /soak/window/report exists.
// This file is created here so Task 5/6/7 can append in order.

#[test]
fn placeholder() {}
```

Actually, the testcontainers path is too heavy for a per-task TDD loop here. Instead, use a lightweight unit test on the JSON shape of the `SoakState::start` handler against an in-memory `Store` substitute. Replace the file contents with:

```rust
//! Unit tests for the soak window handler payload shapes.
//!
//! These tests stub the Redis surface so we don't need testcontainers in
//! the per-task loop. End-to-end coverage of /soak/window comes via the
//! Python orchestrator under `tests/e2e/client/soak/`.

use e2e_fake_web::soak::{SoakSnapshot, WindowStartReq};

#[test]
fn window_start_req_round_trips() {
    let raw = r#"{"user_ids":["u-1","u-2"]}"#;
    let req: WindowStartReq = serde_json::from_str(raw).unwrap();
    assert_eq!(req.user_ids, vec!["u-1".to_string(), "u-2".to_string()]);
}

#[test]
fn snapshot_serialises_with_required_fields() {
    let snap = SoakSnapshot {
        snapshot_id: "abc123".into(),
        t0_ms: 1_716_412_200_000,
        records_xlen: 0,
        dlq_xlen: 0,
        detailed_xlen: 0,
        events_entity_xlen: 0,
        detailed_last_id: "0-0".into(),
        balance_micros: std::collections::HashMap::from([("u-1".into(), 1_000_000_000_000)]),
    };
    let v = serde_json::to_value(&snap).unwrap();
    assert_eq!(v["snapshot_id"], "abc123");
    assert_eq!(v["t0_ms"], 1_716_412_200_000_i64);
    assert_eq!(v["balance_micros"]["u-1"], 1_000_000_000_000_i64);
}
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cargo test -p e2e-fake-web --test soak_window
```

Expected: build error — `e2e-fake-web` lib does not exist (it's currently a bin-only crate); module `soak` not found; types not defined.

- [ ] **Step 3: Convert `e2e-fake-web` to lib+bin**

The crate currently exposes `pub mod` from `src/main.rs` indirectly via the `e2e_fake_web` crate name (already used by `tests/e2e/docker-compose.yml`'s `fake_web` binary). Add an explicit lib target.

Edit `crates/e2e-fake-web/Cargo.toml`:

```toml
[lib]
name = "e2e_fake_web"
path = "src/lib.rs"

[[bin]]
name = "fake_web"
path = "src/main.rs"
```

(The current `Cargo.toml` only has `[[bin]]`. Add `[lib]` ABOVE it.)

The library code already lives in `src/lib.rs` (verified at start of plan). Confirm with:

```bash
cargo build -p e2e-fake-web
```

- [ ] **Step 4: Create the `soak` module**

Create `crates/e2e-fake-web/src/soak.rs`:

```rust
//! Soak window state + start endpoint (spec §5.1).
//!
//! `start` records XLEN snapshots of four Redis streams + per-user balance
//! values + the last id of `usage:detailed`. `report` (Task 7) consumes the
//! snapshot to produce window-delta aggregates.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use fred::interfaces::StreamsInterface;
use fred::types::{XRange, XID};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::store::Store;
use crate::AppState;

/// In-memory table of active snapshots, keyed by `snapshot_id`.
///
/// Each `start` allocates a new entry; `report` looks it up and frees it on
/// success. The table is small (one entry per active soak run — usually 0
/// or 1) so a `tokio::sync::Mutex<HashMap>` is fine.
#[derive(Default)]
pub struct SoakTable {
    inner: Mutex<HashMap<String, SoakSnapshot>>,
}

impl SoakTable {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn insert(&self, snap: SoakSnapshot) {
        let mut t = self.inner.lock().await;
        t.insert(snap.snapshot_id.clone(), snap);
    }
    pub async fn get(&self, id: &str) -> Option<SoakSnapshot> {
        let t = self.inner.lock().await;
        t.get(id).cloned()
    }
    pub async fn remove(&self, id: &str) -> Option<SoakSnapshot> {
        let mut t = self.inner.lock().await;
        t.remove(id)
    }
}

/// Snapshot captured at `POST /soak/window/start`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SoakSnapshot {
    pub snapshot_id: String,
    pub t0_ms: i64,
    pub records_xlen: u64,
    pub dlq_xlen: u64,
    pub detailed_xlen: u64,
    pub events_entity_xlen: u64,
    pub detailed_last_id: String,
    pub balance_micros: HashMap<String, i64>,
}

#[derive(Debug, Deserialize)]
pub struct WindowStartReq {
    #[serde(default)]
    pub user_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WindowStartResp {
    pub snapshot_id: String,
    pub t0_ms: i64,
}

/// Allocate a fresh snapshot id. Hex of (millis<<16 | counter). Not
/// cryptographically random — we only need uniqueness within one
/// orchestrator's lifetime, and snapshots auto-evict on `report`.
fn alloc_snapshot_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    format!("{:x}-{:x}", now, n)
}

pub async fn start(
    State(state): State<AppState>,
    Json(req): Json<WindowStartReq>,
) -> Result<Json<WindowStartResp>, (StatusCode, String)> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let snapshot_id = alloc_snapshot_id();
    let snap = capture_snapshot(&state.store, &snapshot_id, now_ms, &req.user_ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("snapshot: {e}")))?;

    state.soak.insert(snap.clone()).await;
    Ok(Json(WindowStartResp { snapshot_id, t0_ms: now_ms }))
}

async fn capture_snapshot(
    store: &Arc<Store>,
    snapshot_id: &str,
    t0_ms: i64,
    user_ids: &[String],
) -> anyhow::Result<SoakSnapshot> {
    let redis = store.redis_pool();

    let records_xlen: u64 = redis.xlen("usage:records").await.unwrap_or(0);
    let dlq_xlen: u64 = redis.xlen("usage:records:dlq").await.unwrap_or(0);
    let detailed_xlen: u64 = redis.xlen("usage:detailed").await.unwrap_or(0);
    let events_entity_xlen: u64 = redis.xlen("events:entity").await.unwrap_or(0);

    // XRANGE usage:detailed + + COUNT 1 to get the last id (or "0-0" if empty).
    let last_id: String = if detailed_xlen == 0 {
        "0-0".into()
    } else {
        let entries: Vec<(String, Vec<(String, String)>)> = redis
            .xrevrange("usage:detailed", "+", "-", Some(1))
            .await
            .unwrap_or_default();
        entries.into_iter().next().map(|(id, _)| id).unwrap_or_else(|| "0-0".into())
    };

    let mut balance_micros = HashMap::with_capacity(user_ids.len());
    for u in user_ids {
        let key = format!("counter:user:{u}:balance_micro");
        let v: i64 = redis.get(&key).await.unwrap_or(0);
        balance_micros.insert(u.clone(), v);
    }

    Ok(SoakSnapshot {
        snapshot_id: snapshot_id.to_string(),
        t0_ms,
        records_xlen,
        dlq_xlen,
        detailed_xlen,
        events_entity_xlen,
        detailed_last_id: last_id,
        balance_micros,
    })
}
```

(`store.redis_pool()` and `XRange`/`XID` API names match `fred 9.x` — Task 6 / 7 will use the same accessor. If `Store` doesn't currently expose `redis_pool()`, add a `pub fn redis_pool(&self) -> &Arc<RedisPool>` accessor in `crates/e2e-fake-web/src/store.rs` returning the existing field.)

- [ ] **Step 5: Extend `AppState` and `router`**

In `crates/e2e-fake-web/src/lib.rs`:

```rust
pub mod soak;

use soak::SoakTable;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub drain_cursor: Arc<Mutex<String>>,
    pub soak: Arc<SoakTable>,
}

impl AppState {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            drain_cursor: Arc::new(Mutex::new(String::from("0-0"))),
            soak: Arc::new(SoakTable::new()),
        }
    }
}
```

In the same `router` function, register the new route:

```rust
        .route("/soak/window/start", post(soak::start))
```

- [ ] **Step 6: Run the unit test + clippy**

```bash
cargo test -p e2e-fake-web --test soak_window
cargo clippy -p e2e-fake-web --all-targets -- -D warnings
```

Expected: PASS, no warnings. Build will surface any `fred` API mismatch — fix the call to match the workspace `fred = 9.x` API (e.g. `redis.xlen("...").await` may need explicit type or `()` group args).

- [ ] **Step 7: Commit**

```bash
git add crates/e2e-fake-web
git commit -m "soak(fake-web): SoakState + POST /soak/window/start

Captures XLEN snapshots of usage:records / dlq / detailed /
events:entity + last id of usage:detailed + per-user balance
counters. Snapshot is stored in-process keyed by snapshot_id;
report endpoint (next task) consumes it for delta computation."
```

---

### Task 6: Drain-quiescence polling helper

**Files:**
- Modify: `crates/e2e-fake-web/src/soak.rs` (add `wait_for_quiescence`)
- Test: append to `crates/e2e-fake-web/tests/soak_window.rs`

Per spec §5.2. `wait_for_quiescence` polls `XLEN usage:records` every 1 s; once 10 consecutive samples (`quiescence_s=10`) are identical, mark quiescent. Cap at `drain_grace_max_s=60`.

- [ ] **Step 1: Write a unit test for the polling state machine**

The Redis read is non-trivial to mock; extract the *decision logic* into a pure helper that takes a closure returning XLEN, then test that helper.

Append to `crates/e2e-fake-web/tests/soak_window.rs`:

```rust
use e2e_fake_web::soak::quiescence_check;

#[test]
fn quiescence_check_accepts_consecutive_match() {
    let series = vec![100, 105, 110, 110, 110, 110, 110, 110, 110, 110, 110, 110];
    let (quiescent, last) = quiescence_check(&series, 10);
    assert!(quiescent);
    assert_eq!(last, 110);
}

#[test]
fn quiescence_check_rejects_recent_drift() {
    let series = vec![100, 110, 110, 110, 110, 110, 110, 110, 110, 110, 111];
    let (quiescent, _) = quiescence_check(&series, 10);
    assert!(!quiescent);
}

#[test]
fn quiescence_check_empty_or_short_rejects() {
    let (q, _) = quiescence_check(&[], 10);
    assert!(!q);
    let (q, _) = quiescence_check(&[100, 100, 100], 10);
    assert!(!q);
}
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cargo test -p e2e-fake-web --test soak_window quiescence_check
```

Expected: function not defined.

- [ ] **Step 3: Implement `quiescence_check` + `wait_for_quiescence`**

Append to `crates/e2e-fake-web/src/soak.rs`:

```rust
use std::time::{Duration, Instant};

/// Pure decision: do the last `window` entries of `series` all match?
///
/// Returns `(quiescent, last_value)`. `last_value` is undefined when not
/// quiescent (we return the most recent observation for caller's logging).
pub fn quiescence_check(series: &[u64], window: usize) -> (bool, u64) {
    if series.len() < window {
        return (false, *series.last().unwrap_or(&0));
    }
    let tail = &series[series.len() - window..];
    let first = tail[0];
    (tail.iter().all(|&v| v == first), *series.last().unwrap())
}

/// Poll `XLEN usage:records` once per second; return when the last
/// `quiescence_s` samples match, or when `max_wait_s` elapses.
///
/// Returns `(quiescent_reached, observed_series)`. The series is the
/// full observation list — the caller writes it into the report's
/// `drain_observation_series` field.
pub async fn wait_for_quiescence(
    store: &Arc<Store>,
    quiescence_s: usize,
    max_wait_s: u64,
) -> (bool, Vec<u64>) {
    let mut series: Vec<u64> = Vec::with_capacity(max_wait_s as usize + 1);
    let deadline = Instant::now() + Duration::from_secs(max_wait_s);
    let redis = store.redis_pool();
    loop {
        let xlen: u64 = redis.xlen("usage:records").await.unwrap_or(0);
        series.push(xlen);
        let (quiescent, _) = quiescence_check(&series, quiescence_s);
        if quiescent {
            return (true, series);
        }
        if Instant::now() >= deadline {
            return (false, series);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
```

- [ ] **Step 4: Run the test + clippy**

```bash
cargo test -p e2e-fake-web --test soak_window
cargo clippy -p e2e-fake-web --all-targets -- -D warnings
```

Expected: 5 tests PASS, no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/e2e-fake-web/src/soak.rs crates/e2e-fake-web/tests/soak_window.rs
git commit -m "soak(fake-web): drain-quiescence polling helper

quiescence_check is the pure decision (last N samples match).
wait_for_quiescence wraps XLEN polling + 1s tick + 60s ceiling.
Used by the report endpoint to block until usage:records has
stopped growing post-driver (spec §5.2)."
```

---

### Task 7: Detailed-log spectrum + `GET /soak/window/report`

**Files:**
- Modify: `crates/e2e-fake-web/src/soak.rs` (spectrum aggregation + report handler)
- Modify: `crates/e2e-fake-web/src/lib.rs` (mount route)
- Test: append to `crates/e2e-fake-web/tests/soak_window.rs`

Per spec §5.1 + §5.3. `XRANGE usage:detailed <last_id_exclusive> +` walks the entries written during the window; each entry is parsed for `status`, `error_code`, `protocol`, `stream`, `protocol_conversion`, bucketed into the report's `detailed_log_spectrum`. Missing fields go to `__absent__` buckets.

- [ ] **Step 1: Write a unit test for the spectrum aggregator**

Append to `tests/soak_window.rs`:

```rust
use e2e_fake_web::soak::aggregate_spectrum;

#[test]
fn spectrum_buckets_observed_fields_and_marks_absent() {
    let entries: Vec<Vec<(String, String)>> = vec![
        vec![
            ("status".into(), "200".into()),
            ("error_code".into(), "".into()),
            ("protocol".into(), "open_ai".into()),
            ("stream".into(), "true".into()),
            ("protocol_conversion".into(), "false".into()),
        ],
        vec![
            ("status".into(), "429".into()),
            ("error_code".into(), "GW_UPSTREAM_OVERLOADED".into()),
            ("protocol".into(), "anthropic".into()),
            ("stream".into(), "false".into()),
            ("protocol_conversion".into(), "true".into()),
        ],
        vec![
            // missing protocol + stream
            ("status".into(), "502".into()),
            ("error_code".into(), "GW_UPSTREAM_5XX".into()),
        ],
    ];
    let spec = aggregate_spectrum(&entries);
    assert_eq!(spec.by_status.get("200"), Some(&1));
    assert_eq!(spec.by_status.get("429"), Some(&1));
    assert_eq!(spec.by_status.get("502"), Some(&1));
    assert_eq!(spec.by_error_code.get("GW_UPSTREAM_OVERLOADED"), Some(&1));
    assert_eq!(spec.by_error_code.get("__absent__"), Some(&1));
    assert_eq!(spec.by_protocol.get("__absent__"), Some(&1));
    assert_eq!(spec.by_shape.get("stream"), Some(&1));
    assert_eq!(spec.by_shape.get("non_stream"), Some(&1));
    assert_eq!(spec.by_shape.get("__absent__"), Some(&1));
    assert_eq!(spec.by_cross_proto.get("true"), Some(&1));
    assert_eq!(spec.by_cross_proto.get("false"), Some(&1));
}
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cargo test -p e2e-fake-web --test soak_window spectrum_buckets
```

Expected: `aggregate_spectrum` not defined.

- [ ] **Step 3: Implement `DetailedSpectrum` + `aggregate_spectrum`**

Append to `crates/e2e-fake-web/src/soak.rs`:

```rust
/// Spectrum view of `usage:detailed` entries, grouped by the fields the
/// gateway already emits. Missing fields are bucketed as `__absent__` —
/// no silent default per spec C4 (truthfulness).
#[derive(Default, Debug, Serialize)]
pub struct DetailedSpectrum {
    pub by_status: HashMap<String, u64>,
    pub by_error_code: HashMap<String, u64>,
    pub by_protocol: HashMap<String, u64>,
    pub by_shape: HashMap<String, u64>,
    pub by_cross_proto: HashMap<String, u64>,
}

pub fn aggregate_spectrum(entries: &[Vec<(String, String)>]) -> DetailedSpectrum {
    let mut out = DetailedSpectrum::default();
    for fields in entries {
        let get = |k: &str| -> Option<&str> {
            fields.iter().find_map(|(fk, fv)| (fk == k).then_some(fv.as_str()))
        };

        bucket(&mut out.by_status, get("status"));
        bucket(&mut out.by_error_code, get("error_code").filter(|s| !s.is_empty()));
        bucket(&mut out.by_protocol, get("protocol"));
        bucket(&mut out.by_shape, get("stream").map(|s| match s {
            "true" => "stream",
            "false" => "non_stream",
            other => other,
        }));
        bucket(&mut out.by_cross_proto, get("protocol_conversion"));
    }
    out
}

fn bucket(map: &mut HashMap<String, u64>, value: Option<&str>) {
    let key = value.unwrap_or("__absent__");
    *map.entry(key.to_string()).or_insert(0) += 1;
}
```

- [ ] **Step 4: Implement the `/soak/window/report` handler**

Also append to `crates/e2e-fake-web/src/soak.rs`:

```rust
#[derive(Debug, Deserialize)]
pub struct WindowReportQ {
    pub snapshot_id: String,
}

#[derive(Debug, Serialize)]
pub struct WindowReport {
    pub snapshot_id: String,
    pub window_ms: u64,
    pub drain_quiescent: bool,
    pub drain_observation_series: Vec<u64>,
    pub redis_streams: RedisStreamDeltas,
    pub balance_decrement_micros: HashMap<String, i64>,
    pub detailed_log_spectrum: DetailedSpectrum,
}

#[derive(Debug, Serialize)]
pub struct RedisStreamDeltas {
    pub usage_records_delta: u64,
    pub usage_records_dlq_delta: u64,
    pub usage_detailed_delta: u64,
    pub events_entity_delta: u64,
}

pub async fn report(
    State(state): State<AppState>,
    Query(q): Query<WindowReportQ>,
) -> Result<Json<WindowReport>, (StatusCode, String)> {
    let snap = state.soak.get(&q.snapshot_id).await.ok_or((
        StatusCode::NOT_FOUND,
        format!("snapshot_id not found: {}", q.snapshot_id),
    ))?;

    // Block until usage:records has been quiescent for 10s (max 60s).
    let (drain_quiescent, drain_observation_series) =
        wait_for_quiescence(&state.store, 10, 60).await;

    let redis = state.store.redis_pool();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let window_ms = (now_ms - snap.t0_ms).max(0) as u64;

    let records_xlen: u64 = redis.xlen("usage:records").await.unwrap_or(0);
    let dlq_xlen: u64 = redis.xlen("usage:records:dlq").await.unwrap_or(0);
    let detailed_xlen: u64 = redis.xlen("usage:detailed").await.unwrap_or(0);
    let events_entity_xlen: u64 = redis.xlen("events:entity").await.unwrap_or(0);

    // XRANGE usage:detailed (last_id, +] — entries written during the window.
    // `(<id>` is fred's exclusive start syntax for streams.
    let after = format!("({}", snap.detailed_last_id);
    let xrange: Vec<(String, Vec<(String, String)>)> = redis
        .xrange("usage:detailed", &after, "+", None)
        .await
        .unwrap_or_default();
    let entries_only: Vec<Vec<(String, String)>> = xrange.into_iter().map(|(_, e)| e).collect();
    let spectrum = aggregate_spectrum(&entries_only);

    // Balance decrements: snapshot - current. Positive means money spent.
    let mut balance_decrement_micros = HashMap::with_capacity(snap.balance_micros.len());
    for (u, before) in &snap.balance_micros {
        let key = format!("counter:user:{u}:balance_micro");
        let after_val: i64 = redis.get(&key).await.unwrap_or(0);
        balance_decrement_micros.insert(u.clone(), before - after_val);
    }

    // Free the snapshot — done with it.
    let _ = state.soak.remove(&q.snapshot_id).await;

    Ok(Json(WindowReport {
        snapshot_id: q.snapshot_id,
        window_ms,
        drain_quiescent,
        drain_observation_series,
        redis_streams: RedisStreamDeltas {
            usage_records_delta: records_xlen.saturating_sub(snap.records_xlen),
            usage_records_dlq_delta: dlq_xlen.saturating_sub(snap.dlq_xlen),
            usage_detailed_delta: detailed_xlen.saturating_sub(snap.detailed_xlen),
            events_entity_delta: events_entity_xlen.saturating_sub(snap.events_entity_xlen),
        },
        balance_decrement_micros,
        detailed_log_spectrum: spectrum,
    }))
}
```

- [ ] **Step 5: Mount the route**

In `crates/e2e-fake-web/src/lib.rs::router`:

```rust
        .route("/soak/window/report", get(soak::report))
```

- [ ] **Step 6: Run tests + clippy**

```bash
cargo test -p e2e-fake-web --test soak_window
cargo clippy -p e2e-fake-web --all-targets -- -D warnings
```

Expected: 6 tests PASS, no warnings. If `fred`'s `xrange`/`xrevrange` typing requires explicit turbofish, add it; the workspace `fred = 9` API uses `xrange<R, K, S, E>(key, start, end, count) -> R` so the explicit `Vec<(...)>` annotation in the binding usually drives inference.

- [ ] **Step 7: Commit**

```bash
git add crates/e2e-fake-web
git commit -m "soak(fake-web): GET /soak/window/report

Composes XLEN deltas + balance decrements + detailed-log spectrum
(XRANGE since snapshot's last_id, bucket by status/error_code/
protocol/stream/protocol_conversion with __absent__ for missing
fields per spec C4). Blocks on drain quiescence before computing
deltas so account-consistency invariant is meaningful."
```

---

## Phase 3 — Python soak module primitives

### Task 8: `codes_catalog.py` + `chaos_profiles.py`

**Files:**
- Create: `tests/e2e/client/soak/__init__.py` (empty marker)
- Create: `tests/e2e/client/soak/codes_catalog.py`
- Create: `tests/e2e/client/soak/chaos_profiles.py`
- Test: `tests/e2e/client/soak/test_primitives.py`

Per spec §7.1.4 (catalog grep) + §11.1 (pinned weights). LIGHT/HEAVY profiles are immutable tuples here, not "tune-later" placeholders.

- [ ] **Step 1: Write the failing test**

Create `tests/e2e/client/soak/test_primitives.py`:

```python
"""Unit tests for the soak primitive modules (no docker, no live services)."""
from pathlib import Path

from soak.chaos_profiles import HEAVY_PROFILE, LIGHT_PROFILE
from soak.codes_catalog import parse_codes_catalog


REPO_ROOT = Path(__file__).resolve().parents[4]  # tests/e2e/client/soak → repo


def test_chaos_profiles_weights_sum_to_100():
    # Per spec §11.1 LIGHT 96/1/1/1/1/0/0 and HEAVY 80/5/5/3/2/3/2 each sum
    # to 100. This is a structural invariant — we use the values directly as
    # percentages in dispatch_stats so a typo silently skewing the soak's
    # bug-finding density is unacceptable.
    assert sum(v["weight"] for v in LIGHT_PROFILE) == 100
    assert sum(v["weight"] for v in HEAVY_PROFILE) == 100


def test_chaos_profiles_first_variant_is_sse_happy():
    # variants[0] is the happy path (spec §4.1) — chaos-off falls through here.
    for p in (LIGHT_PROFILE, HEAVY_PROFILE):
        assert p[0]["spec"]["kind"] == "sse"


def test_codes_catalog_parses_known_codes():
    codes_rs = REPO_ROOT / "crates" / "gw-core" / "src" / "error" / "codes.rs"
    catalog = parse_codes_catalog(codes_rs)
    # Sanity checks against the constants observed in the spec writing
    # session (see Appendix A in the design spec).
    expected = {
        "GW_UPSTREAM_TIMEOUT",
        "GW_UPSTREAM_5XX",
        "GW_UPSTREAM_OVERLOADED",
        "GW_UPSTREAM_CONNECT_FAILED",
        "GW_UPSTREAM_CANCELLED",
        "GW_AUTH_INVALID_APIKEY",
        "GW_REQUEST_MODEL_NOT_FOUND",
    }
    missing = expected - catalog
    assert not missing, f"catalog missing expected codes: {missing}"
    # All entries match the GW_* prefix and uppercase shape.
    for c in catalog:
        assert c.startswith("GW_") and c.isupper(), c
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_primitives.py -v
```

Expected: `ModuleNotFoundError: No module named 'soak'`.

- [ ] **Step 3: Create `soak/__init__.py`**

Create the empty marker:

```bash
touch tests/e2e/client/soak/__init__.py
```

- [ ] **Step 4: Create `codes_catalog.py`**

Create `tests/e2e/client/soak/codes_catalog.py`:

```python
"""Parse `crates/gw-core/src/error/codes.rs` for the set of GW_* error codes.

Used at orchestrator startup to validate (a) the variant→error_code
mapping in the spec's Appendix A, and (b) the all-codes-in-catalog
invariant (§7.1.4). We deliberately parse the source file at runtime
rather than embed a snapshot, so catalog drift fails the soak loudly
(per spec C6: no calibration phase, no silent staleness).
"""
from __future__ import annotations

import re
from pathlib import Path

_CONST_LINE = re.compile(r"^pub const (GW_[A-Z_]+)\s*:")


def parse_codes_catalog(codes_rs_path: Path) -> set[str]:
    """Return the set of GW_* constants defined in `codes_rs_path`.

    Raises FileNotFoundError if the file does not exist (this is a hard
    fail: the soak cannot run without the catalog).
    """
    text = codes_rs_path.read_text(encoding="utf-8")
    out: set[str] = set()
    for line in text.splitlines():
        m = _CONST_LINE.match(line.strip())
        if m:
            out.add(m.group(1))
    if not out:
        raise RuntimeError(
            f"no GW_* constants parsed from {codes_rs_path} — file shape changed?"
        )
    return out
```

- [ ] **Step 5: Create `chaos_profiles.py`**

Create `tests/e2e/client/soak/chaos_profiles.py`:

```python
"""Pinned chaos scenario profiles — see spec §11.1.

Per spec C6, these values are immutable. Changing them requires editing
the spec first; the comment above each PROFILE references the spec section.
"""
from __future__ import annotations

# A single happy SSE response that mirrors `scenario.rs::default_scenario`
# (2 events: a content delta + [DONE]). Used as variants[0] of every
# Weighted route — chaos-off mode falls through to it (spec §4.1).
_HAPPY_SSE = {
    "kind": "sse",
    "delay_ms": 0,
    "events": [
        {"data": '{"choices":[{"delta":{"content":"hi"}}]}'},
        {"data": "[DONE]"},
    ],
}


def _variant(weight: int, spec: dict) -> dict:
    return {"weight": weight, "spec": spec}


# Spec §11.1 — LIGHT: 96 % happy, 4 % faults distributed across partial /
# timeout / 429 / mid-body failures. Connection-class faults are reserved
# for HEAVY so the two profiles exercise different gateway paths.
LIGHT_PROFILE: list[dict] = [
    _variant(96, _HAPPY_SSE),
    _variant(1, {"kind": "tcp_reset_mid_body", "after_bytes": 256}),
    _variant(1, {"kind": "status_with_retry_after",
                 "status": 429, "retry_after_secs": 1,
                 "body": '{"error":"rate-limited"}'}),
    _variant(1, {"kind": "slow_body_byte_per_second",
                 "body": '{"choices":[{"delta":{"content":"slow"}}]}',
                 "bps": 4}),
    _variant(1, {"kind": "partial_sse_event", "half_body": 'data: {"choi'}),
    _variant(0, {"kind": "tcp_reset_after_headers"}),
    _variant(0, {"kind": "static", "status": 502,
                 "headers": {"content-type": "application/json"},
                 "body": '{"error":"bad gateway"}',
                 "delay_ms": 0}),
]

# Spec §11.1 — HEAVY: 80 % happy, 20 % faults skewed toward connection-class
# and rate-limit faults so retry / cooldown / health-degrade paths fire.
HEAVY_PROFILE: list[dict] = [
    _variant(80, _HAPPY_SSE),
    _variant(5, {"kind": "tcp_reset_mid_body", "after_bytes": 256}),
    _variant(5, {"kind": "status_with_retry_after",
                 "status": 429, "retry_after_secs": 1,
                 "body": '{"error":"rate-limited"}'}),
    _variant(3, {"kind": "slow_body_byte_per_second",
                 "body": '{"choices":[{"delta":{"content":"slow"}}]}',
                 "bps": 4}),
    _variant(2, {"kind": "partial_sse_event", "half_body": 'data: {"choi'}),
    _variant(3, {"kind": "tcp_reset_after_headers"}),
    _variant(2, {"kind": "static", "status": 502,
                 "headers": {"content-type": "application/json"},
                 "body": '{"error":"bad gateway"}',
                 "delay_ms": 0}),
]


def profile_for(mode: str) -> list[dict] | None:
    """Return the profile for `mode`, or `None` for `'off'`.

    `'off'` means: do not POST any weighted scenario; let fake-upstream
    serve its default happy SSE. (Equivalent to also POSTing a degenerate
    `[(100, sse_happy)]` Weighted, but skipping is simpler.)
    """
    if mode == "off":
        return None
    if mode == "light":
        return LIGHT_PROFILE
    if mode == "heavy":
        return HEAVY_PROFILE
    raise ValueError(f"unknown chaos mode: {mode}")


def to_scenario(profile: list[dict], path_regex: str = "^/v1/chat/completions$") -> dict:
    """Wrap a profile into the full Scenario JSON the fake-upstream
    admin endpoint accepts. Only one route is needed — chaos in soak
    is global, not per-path."""
    return {
        "routes": [
            {
                "match": {"method": "POST", "path_regex": path_regex},
                "response": {"kind": "weighted", "variants": profile},
            }
        ]
    }
```

- [ ] **Step 6: Run the test**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_primitives.py -v
```

Expected: 3 PASS.

- [ ] **Step 7: Commit**

```bash
git add tests/e2e/client/soak/__init__.py tests/e2e/client/soak/codes_catalog.py tests/e2e/client/soak/chaos_profiles.py tests/e2e/client/soak/test_primitives.py
git commit -m "soak(py): codes_catalog parser + pinned chaos profiles

LIGHT/HEAVY weight tables match spec §11.1 exactly (sum=100).
codes_catalog.parse_codes_catalog returns the GW_* set from
crates/gw-core/src/error/codes.rs at runtime (spec §7.1.4) — no
embedded snapshot so catalog drift fails the soak immediately."
```

---

### Task 9: `seed.py` — bulk seed for 3-protocol + cross-proto graph

**Files:**
- Create: `tests/e2e/client/soak/seed.py`
- Test: append to `tests/e2e/client/soak/test_primitives.py`

Per spec §6.4. Seeds 20 users + 20 api_keys + 3 same-proto vendor_keys + 1 cross-proto vendor_key + 4 model_groups + 3 pricings. All model_groups use `strategy="fixed_order"` + `cross_vendor_fallback=false` to keep 1 client request ⇒ ≤1 upstream call (C5).

- [ ] **Step 1: Write the failing test**

Append to `tests/e2e/client/soak/test_primitives.py`:

```python
from soak.seed import SOAK_MODEL_OPENAI, SOAK_MODEL_ANTHROPIC, SOAK_MODEL_GEMINI, SoakSeedHandles


def test_seed_handles_shape():
    # Pure-data sanity: handles dataclass shape that the driver consumes.
    h = SoakSeedHandles(
        users=[("u-1", "ak-1", "sk-1"), ("u-2", "ak-2", "sk-2")],
        groups_by_kind={
            "open_ai": "soak-open_ai",
            "anthropic": "soak-anthropic",
            "gemini": "soak-gemini",
            "cross": "soak-cross",
        },
        models_by_kind={
            "open_ai": SOAK_MODEL_OPENAI,
            "anthropic": SOAK_MODEL_ANTHROPIC,
            "gemini": SOAK_MODEL_GEMINI,
        },
    )
    assert len(h.users) == 2
    assert h.groups_by_kind["cross"] == "soak-cross"
    assert h.models_by_kind["open_ai"] == SOAK_MODEL_OPENAI
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_primitives.py::test_seed_handles_shape -v
```

Expected: `ImportError: cannot import name 'SoakSeedHandles' from 'soak.seed'`.

- [ ] **Step 3: Create `seed.py`**

Create `tests/e2e/client/soak/seed.py`:

```python
"""Bulk seed for the soak harness — see spec §6.4.

One-shot population of users / api_keys / vendors / vendor_keys /
model_groups / pricings sufficient to drive 50k+ mixed-protocol
requests through the gateway. Every model_group uses fixed_order
strategy with cross_vendor_fallback=False so retry is disabled and
1 client request ⇒ ≤1 upstream call (spec C5).
"""
from __future__ import annotations

from dataclasses import dataclass

from conftest import (
    BALANCE_LARGE,
    PROTO_OPENAI,
    FakeWebClient,
    seed_model_group,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
)


# Model names per protocol — the gateway uses `model` as the routing key.
SOAK_MODEL_OPENAI = "gpt-4"
SOAK_MODEL_ANTHROPIC = "claude-3-haiku-20240307"
SOAK_MODEL_GEMINI = "gemini-2.0-flash"

PROTO_ANTHROPIC = "anthropic"
PROTO_GEMINI = "gemini"


@dataclass
class SoakSeedHandles:
    """What `seed_soak` returns — the soak driver pulls user/group/model
    references from this dataclass at request-construction time."""
    users: list[tuple[str, str, str]]
    """[(user_id, api_key_id, api_key_plaintext), ...] — 20 entries by default."""
    groups_by_kind: dict[str, str]
    """{'open_ai': 'soak-open_ai', 'anthropic': ..., 'gemini': ..., 'cross': ...}"""
    models_by_kind: dict[str, str]
    """{'open_ai': 'gpt-4', 'anthropic': '...', 'gemini': '...'} — used for
    the request body. Cross requests reuse the OpenAI model name (the
    client sends OpenAI wire but the vendor_key is Anthropic-only)."""


def seed_soak(fake_web: FakeWebClient, n_users: int = 20) -> SoakSeedHandles:
    """Populate the entire entity graph for one soak run.

    Idempotency: the soak fixture wipes state via `fake_web.reset()` BEFORE
    each test (see conftest.isolate_state), so this function assumes a clean
    slate. Callers MUST NOT invoke it twice without an intervening reset.
    """
    # ── 20 users + 20 api_keys, all with the same allowed_groups so the
    # auth merge step doesn't reject any soak request. ────────────────────
    users: list[tuple[str, str, str]] = []
    for i in range(n_users):
        uid = f"u-soak-{i:03d}"
        kid = f"ak-soak-{i:03d}"
        plain = f"sk-soak-{i:03d}"
        seed_user_apikey(
            fake_web,
            user_id=uid,
            api_key_id=kid,
            api_key_plaintext=plain,
            allowed_groups=[
                "soak-open_ai", "soak-anthropic", "soak-gemini", "soak-cross",
            ],
            rpm_limit=100_000,  # well above 833 rps per user
        )
        fake_web.set_balance(uid, BALANCE_LARGE)
        users.append((uid, kid, plain))

    # ── 3 vendors, one per protocol family. ──────────────────────────────
    base = "http://fake-upstream:9000"
    seed_vendor(fake_web, vendor_id="v-soak-open_ai", base_urls=[base])
    seed_vendor(fake_web, vendor_id="v-soak-anthropic", base_urls=[base])
    seed_vendor(fake_web, vendor_id="v-soak-gemini", base_urls=[base])

    # ── 3 same-protocol vendor_keys + 1 cross-protocol vendor_key. ──────
    # Cross-proto: the gateway accepts the request as OpenAI wire but the
    # vendor_key is anthropic-only with protocol_conversion=true, forcing
    # the two-end re-render path. We use `gpt-4` as the model name (same as
    # SOAK_MODEL_OPENAI) so the gateway's model → group lookup picks the
    # cross group.
    seed_vendor_key(
        fake_web,
        key_id="vk-soak-open_ai",
        vendor_id="v-soak-open_ai",
        model=SOAK_MODEL_OPENAI,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_vendor_key(
        fake_web,
        key_id="vk-soak-anthropic",
        vendor_id="v-soak-anthropic",
        model=SOAK_MODEL_ANTHROPIC,
        accessible_endpoints=[PROTO_ANTHROPIC],
    )
    seed_vendor_key(
        fake_web,
        key_id="vk-soak-gemini",
        vendor_id="v-soak-gemini",
        model=SOAK_MODEL_GEMINI,
        accessible_endpoints=[PROTO_GEMINI],
    )
    seed_vendor_key(
        fake_web,
        key_id="vk-soak-cross",
        vendor_id="v-soak-anthropic",
        model=SOAK_MODEL_OPENAI,         # client sends OpenAI wire for `gpt-4`
        accessible_endpoints=[PROTO_ANTHROPIC],
        protocol_conversion=True,         # re-render OpenAI ⇆ Anthropic
    )

    # ── 4 model_groups — each pins one vendor_key, fixed_order, no
    # cross-vendor-fallback. C5: retry disabled, 1 request ⇒ ≤1 upstream
    # call so account-consistency invariant can be strict ==. ─────────────
    for group, key in [
        ("soak-open_ai", "vk-soak-open_ai"),
        ("soak-anthropic", "vk-soak-anthropic"),
        ("soak-gemini", "vk-soak-gemini"),
        ("soak-cross", "vk-soak-cross"),
    ]:
        seed_model_group(
            fake_web,
            group=group,
            vendor_key_ids=[key],
            strategy="fixed_order",
            cross_vendor_fallback=False,
        )

    # ── 3 pricings — one per protocol model. ─────────────────────────────
    for m in (SOAK_MODEL_OPENAI, SOAK_MODEL_ANTHROPIC, SOAK_MODEL_GEMINI):
        seed_pricing(fake_web, model=m)

    return SoakSeedHandles(
        users=users,
        groups_by_kind={
            "open_ai": "soak-open_ai",
            "anthropic": "soak-anthropic",
            "gemini": "soak-gemini",
            "cross": "soak-cross",
        },
        models_by_kind={
            "open_ai": SOAK_MODEL_OPENAI,
            "anthropic": SOAK_MODEL_ANTHROPIC,
            "gemini": SOAK_MODEL_GEMINI,
        },
    )
```

- [ ] **Step 4: Run the test**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_primitives.py::test_seed_handles_shape -v
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/e2e/client/soak/seed.py tests/e2e/client/soak/test_primitives.py
git commit -m "soak(py): seed.py — 20 users × 3 vendors × 4 groups

One-shot entity graph for the soak harness. All groups use
fixed_order + cross_vendor_fallback=False so retry is disabled
(spec C5: enables strict-equality account-consistency invariant)."
```

---

### Task 10: `workload.py` — request-spec sampling + 6 wire builders

**Files:**
- Create: `tests/e2e/client/soak/workload.py`
- Test: append to `tests/e2e/client/soak/test_primitives.py`

Per spec §6.3. The driver does NOT call the OpenAI/Anthropic/Gemini SDKs at request rate — instead httpx posts the wire JSON directly. Six minimal builders cover the 3 protocols × 2 shapes.

- [ ] **Step 1: Write the failing test**

Append to `tests/e2e/client/soak/test_primitives.py`:

```python
import random

from soak.workload import (
    RequestSpec,
    WorkloadMix,
    build_request,
    sample_request_spec,
)
from soak.seed import SoakSeedHandles, SOAK_MODEL_OPENAI


def _fake_handles() -> SoakSeedHandles:
    return SoakSeedHandles(
        users=[("u-1", "ak-1", "sk-1"), ("u-2", "ak-2", "sk-2")],
        groups_by_kind={
            "open_ai": "g-o", "anthropic": "g-a", "gemini": "g-g", "cross": "g-c",
        },
        models_by_kind={
            "open_ai": SOAK_MODEL_OPENAI,
            "anthropic": "claude-3-haiku-20240307",
            "gemini": "gemini-2.0-flash",
        },
    )


def test_sample_distribution_respects_weights():
    rng = random.Random(42)
    mix = WorkloadMix.default()
    h = _fake_handles()
    counts: dict[str, int] = {}
    for _ in range(10_000):
        spec = sample_request_spec(mix, h, rng)
        counts[spec.protocol] = counts.get(spec.protocol, 0) + 1
    # Each protocol gets ~33 % — 10k samples gives ±2 % wiggle.
    for proto in ("open_ai", "anthropic", "gemini"):
        pct = counts.get(proto, 0) / 10_000 * 100
        assert 28 <= pct <= 38, f"{proto} pct={pct:.1f}"


def test_build_request_openai_stream():
    h = _fake_handles()
    spec = RequestSpec(
        protocol="open_ai",
        shape="stream",
        cross_protocol=False,
        payload_size_bucket="small",
        user_id="u-1",
        api_key_id="ak-1",
        api_key_plaintext="sk-1",
    )
    req = build_request(spec, h)
    assert req["method"] == "POST"
    assert req["path"] == "/v1/chat/completions"
    assert req["headers"]["x-api-key"] == "sk-1"
    assert req["headers"]["x-api-key-id"] == "ak-1"
    assert req["json"]["model"] == SOAK_MODEL_OPENAI
    assert req["json"]["stream"] is True


def test_build_request_anthropic_non_stream():
    h = _fake_handles()
    spec = RequestSpec(
        protocol="anthropic",
        shape="non_stream",
        cross_protocol=False,
        payload_size_bucket="small",
        user_id="u-1",
        api_key_id="ak-1",
        api_key_plaintext="sk-1",
    )
    req = build_request(spec, h)
    assert req["path"] == "/v1/messages"
    assert req["json"]["model"] == "claude-3-haiku-20240307"
    assert req["json"].get("stream") in (False, None)


def test_build_request_cross_protocol_targets_cross_group():
    h = _fake_handles()
    spec = RequestSpec(
        protocol="open_ai",
        shape="stream",
        cross_protocol=True,
        payload_size_bucket="small",
        user_id="u-1",
        api_key_id="ak-1",
        api_key_plaintext="sk-1",
    )
    req = build_request(spec, h)
    # Cross-proto: client sends OpenAI wire but the routing key (model
    # name) maps to the cross group via the gateway's model->group lookup.
    # We model this by sending `gpt-4` (same as OpenAI), trusting that
    # seed.py wired vk-soak-cross under the soak-cross group with this
    # model name.
    assert req["path"] == "/v1/chat/completions"
    assert req["json"]["model"] == SOAK_MODEL_OPENAI
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_primitives.py -k workload -v
```

Expected: ImportError on `soak.workload`.

- [ ] **Step 3: Create `workload.py`**

Create `tests/e2e/client/soak/workload.py`:

```python
"""Request-spec sampling + 3 protocols × 2 shapes wire builders (spec §6.3).

The driver constructs requests directly with httpx — NOT through SDKs —
because:
  - openai / anthropic / google-genai sync clients don't compose with
    asyncio + 833 rps cleanly;
  - the soak's job is to exercise the gateway, not validate the SDKs
    (which are covered by existing scenario tests).
"""
from __future__ import annotations

import random
from dataclasses import dataclass
from typing import Literal

from soak.seed import SoakSeedHandles


Protocol = Literal["open_ai", "anthropic", "gemini"]
Shape = Literal["stream", "non_stream"]
PayloadBucket = Literal["small", "medium", "large"]


@dataclass(frozen=True)
class RequestSpec:
    protocol: Protocol
    shape: Shape
    cross_protocol: bool
    payload_size_bucket: PayloadBucket
    user_id: str
    api_key_id: str
    api_key_plaintext: str


@dataclass(frozen=True)
class WorkloadMix:
    protocol_weights: dict[Protocol, int]
    shape_weights: dict[Shape, int]
    cross_protocol_pct: int
    payload_size_weights: dict[PayloadBucket, int]

    @classmethod
    def default(cls) -> "WorkloadMix":
        return cls(
            protocol_weights={"open_ai": 33, "anthropic": 33, "gemini": 34},
            shape_weights={"stream": 50, "non_stream": 50},
            cross_protocol_pct=10,
            payload_size_weights={"small": 70, "medium": 25, "large": 5},
        )


def _weighted_choice(rng: random.Random, weights: dict[str, int]) -> str:
    """Sample a key from a dict of {value: weight}. Weights must sum > 0."""
    keys = list(weights.keys())
    vals = list(weights.values())
    return rng.choices(keys, weights=vals, k=1)[0]


def sample_request_spec(
    mix: WorkloadMix,
    handles: SoakSeedHandles,
    rng: random.Random,
) -> RequestSpec:
    proto = _weighted_choice(rng, mix.protocol_weights)
    shape = _weighted_choice(rng, mix.shape_weights)
    cross = (rng.randint(1, 100) <= mix.cross_protocol_pct)
    bucket = _weighted_choice(rng, mix.payload_size_weights)
    uid, kid, plain = rng.choice(handles.users)
    return RequestSpec(
        protocol=proto,  # type: ignore[arg-type]
        shape=shape,     # type: ignore[arg-type]
        cross_protocol=cross,
        payload_size_bucket=bucket,  # type: ignore[arg-type]
        user_id=uid,
        api_key_id=kid,
        api_key_plaintext=plain,
    )


# ── Payload templates ────────────────────────────────────────────────────

def _content_for(bucket: PayloadBucket) -> str:
    """Per spec §6.3, vary payload size across small/medium/large buckets.
    Sizes chosen to be small enough not to swamp gateway parsing but large
    enough to exercise the codec's slice handling."""
    if bucket == "small":
        return "hi"
    if bucket == "medium":
        return "hi " * 64
    return "hi " * 512


def build_request(spec: RequestSpec, handles: SoakSeedHandles) -> dict:
    """Build the wire-side request dict the driver POSTs via httpx.

    Returns: {"method": str, "path": str, "headers": dict, "json": dict}.
    The driver constructs the URL by joining gateway_url + path and POSTs.
    """
    headers = {
        "x-api-key": spec.api_key_plaintext,
        "x-api-key-id": spec.api_key_id,
        "content-type": "application/json",
    }
    content = _content_for(spec.payload_size_bucket)

    if spec.protocol == "open_ai":
        # Cross-proto requests are still wire-OpenAI; the gateway picks the
        # cross vendor_key via model→group routing (seed.py wires the cross
        # group on `gpt-4`).
        model = handles.models_by_kind["open_ai"]
        body = {
            "model": model,
            "messages": [{"role": "user", "content": content}],
            "stream": spec.shape == "stream",
            "max_tokens": 64,
        }
        return {"method": "POST", "path": "/v1/chat/completions",
                "headers": headers, "json": body}

    if spec.protocol == "anthropic":
        model = handles.models_by_kind["anthropic"]
        body = {
            "model": model,
            "messages": [{"role": "user", "content": content}],
            "max_tokens": 64,
        }
        if spec.shape == "stream":
            body["stream"] = True
        return {"method": "POST", "path": "/v1/messages",
                "headers": headers, "json": body}

    # gemini
    model = handles.models_by_kind["gemini"]
    path = (
        f"/v1beta/models/{model}:streamGenerateContent"
        if spec.shape == "stream"
        else f"/v1beta/models/{model}:generateContent"
    )
    body = {
        "contents": [{"parts": [{"text": content}], "role": "user"}],
        "generationConfig": {"maxOutputTokens": 64},
    }
    return {"method": "POST", "path": path, "headers": headers, "json": body}
```

- [ ] **Step 4: Run the workload tests**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_primitives.py -v
```

Expected: all PASS (4 from earlier + 4 new = 8). The distribution test uses a fixed seed (42) and 10k samples so it should be stable.

- [ ] **Step 5: Commit**

```bash
git add tests/e2e/client/soak/workload.py tests/e2e/client/soak/test_primitives.py
git commit -m "soak(py): workload.py — sampling + 6 wire builders

3 protocols × 2 shapes (stream/non-stream) wire builders driven from
RequestSpec sampled per WorkloadMix. Cross-proto requests are
wire-OpenAI; the gateway's model→group lookup picks the cross group
via the model name (seed.py wires vk-soak-cross under soak-cross)."
```

---

## Phase 4 — Python soak driver

### Task 11: Driver dataclasses (SoakConfig, SoakDriverReport)

**Files:**
- Create: `tests/e2e/client/soak/driver.py` (dataclasses + skeleton)
- Test: `tests/e2e/client/soak/test_driver.py`

Per spec §6.2. SoakConfig defaults match §11.3 exactly.

- [ ] **Step 1: Write the failing test**

Create `tests/e2e/client/soak/test_driver.py`:

```python
"""Unit tests for the soak driver scaffolding (no live HTTP)."""
from soak.driver import SoakConfig, SoakDriverReport


def test_soak_config_defaults_match_spec_11_3():
    # Spec §11.3 — full row. Any change requires updating the spec first.
    c = SoakConfig()
    assert c.duration_s == 60
    assert c.target_rps == 833
    assert c.n_threads == 8
    assert c.max_inflight_per_thread == 64
    assert c.per_request_timeout_s == 30
    assert c.drain_grace_s == 5
    assert c.drain_quiescence_s == 10
    assert c.drain_grace_max_s == 60
    assert c.n_users == 20
    assert c.chaos_mode == "off"


def test_soak_config_smoke():
    # Spec §11.3 — smoke column.
    c = SoakConfig.smoke()
    assert c.duration_s == 10
    assert c.target_rps == 100
    assert c.n_threads == 2
    assert c.max_inflight_per_thread == 16
    assert c.n_users == 5


def test_driver_report_target_count_is_product():
    rpt = SoakDriverReport.empty(target_count=60 * 833)
    assert rpt.target_count == 49980
    assert rpt.requests_sent == 0
    assert rpt.requests_completed == 0
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_driver.py -v
```

Expected: `ImportError: cannot import name 'SoakConfig' from 'soak.driver'`.

- [ ] **Step 3: Create the driver scaffolding**

Create `tests/e2e/client/soak/driver.py`:

```python
"""Concurrent async driver for the soak harness — see spec §6.

Architecture: a ThreadPoolExecutor of N OS threads (breaks GIL), each
running its own asyncio loop + httpx.AsyncClient. The main thread runs
a rate emitter that sleeps `1/target_rps` between ticks and dispatches
a freshly-sampled RequestSpec to a round-robin worker queue.

This file is split across Tasks 11–13:
  - Task 11: SoakConfig + SoakDriverReport dataclasses.
  - Task 12: per-thread async runner (`_thread_loop`).
  - Task 13: rate emitter + livez probe + report aggregation.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal


ChaosMode = Literal["off", "light", "heavy"]


@dataclass(frozen=True)
class SoakConfig:
    """Spec §11.3 — pinned defaults. Use SoakConfig.smoke() for CI iter."""
    duration_s: int = 60
    target_rps: int = 833
    n_threads: int = 8
    max_inflight_per_thread: int = 64
    per_request_timeout_s: int = 30
    drain_grace_s: int = 5
    drain_quiescence_s: int = 10
    drain_grace_max_s: int = 60
    n_users: int = 20
    chaos_mode: ChaosMode = "off"
    gateway_url: str = "http://localhost:8000"
    livez_url: str = "http://localhost:8080/livez"
    readyz_url: str = "http://localhost:8080/readyz"

    @property
    def target_count(self) -> int:
        return self.duration_s * self.target_rps

    @classmethod
    def smoke(cls) -> "SoakConfig":
        return cls(
            duration_s=10,
            target_rps=100,
            n_threads=2,
            max_inflight_per_thread=16,
            drain_quiescence_s=5,
            drain_grace_max_s=30,
            n_users=5,
        )


@dataclass
class RequestOutcome:
    protocol: str
    shape: str
    cross_protocol: bool
    status: int | None
    error_code: str | None
    latency_ms: float
    bytes_received: int
    failed_client_side: bool


@dataclass
class SoakDriverReport:
    duration_ms: int
    target_count: int
    requests_sent: int
    requests_completed: int
    requests_failed_client_side: int
    by_status: dict[int, int] = field(default_factory=dict)
    by_error_code: dict[str, int] = field(default_factory=dict)
    by_protocol: dict[str, int] = field(default_factory=dict)
    by_shape: dict[str, int] = field(default_factory=dict)
    by_cross_proto: dict[bool, int] = field(default_factory=dict)
    latency_ms_p50: float | None = None
    latency_ms_p90: float | None = None
    latency_ms_p99: float | None = None
    latency_ms_max: float | None = None
    bytes_received_total: int = 0
    thread_cpu_us_total: dict[int, int] = field(default_factory=dict)
    livez_probes: int = 0
    livez_non_200: int = 0
    readyz_probes: int = 0
    readyz_non_200: int = 0

    @classmethod
    def empty(cls, *, target_count: int) -> "SoakDriverReport":
        return cls(
            duration_ms=0,
            target_count=target_count,
            requests_sent=0,
            requests_completed=0,
            requests_failed_client_side=0,
        )
```

- [ ] **Step 4: Run the test + commit**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_driver.py -v
```

Expected: 3 PASS.

```bash
git add tests/e2e/client/soak/driver.py tests/e2e/client/soak/test_driver.py
git commit -m "soak(py): driver dataclasses pinned to spec §11.3

SoakConfig holds all knobs (no env-driven overrides — spec C6),
SoakDriverReport is the driver-side rollup the orchestrator merges
with fake-web report + /metrics for invariant evaluation."
```

---

### Task 12: Per-thread async runner

**Files:**
- Modify: `tests/e2e/client/soak/driver.py` (add `_run_one_request`, `_thread_loop`)
- Test: append to `tests/e2e/client/soak/test_driver.py`

Per spec §6.1. Each worker thread runs `asyncio.run` over its own loop; receives `RequestSpec` from a `queue.SimpleQueue`, fires the request, records outcome.

- [ ] **Step 1: Write the failing test against a local httpx mock server**

Append to `tests/e2e/client/soak/test_driver.py`:

```python
import asyncio
import threading

import httpx
import pytest

from soak.driver import RequestOutcome, _run_one_request, SoakConfig
from soak.seed import SoakSeedHandles, SOAK_MODEL_OPENAI
from soak.workload import RequestSpec


@pytest.mark.asyncio
async def test_run_one_request_records_status_and_latency(monkeypatch):
    transport = httpx.MockTransport(lambda r: httpx.Response(
        200,
        headers={"content-type": "text/event-stream"},
        text='data: {"choices":[{"delta":{"content":"hi"}}]}\n\ndata: [DONE]\n\n',
    ))
    client = httpx.AsyncClient(transport=transport, timeout=5.0,
                               base_url="http://gateway.local")

    spec = RequestSpec(
        protocol="open_ai", shape="stream", cross_protocol=False,
        payload_size_bucket="small",
        user_id="u-1", api_key_id="ak-1", api_key_plaintext="sk-1",
    )
    handles = SoakSeedHandles(
        users=[("u-1", "ak-1", "sk-1")],
        groups_by_kind={"open_ai": "g", "anthropic": "g", "gemini": "g", "cross": "g"},
        models_by_kind={"open_ai": SOAK_MODEL_OPENAI,
                        "anthropic": "x", "gemini": "y"},
    )

    out = await _run_one_request(client, spec, handles, timeout_s=5)
    assert isinstance(out, RequestOutcome)
    assert out.status == 200
    assert out.failed_client_side is False
    assert out.latency_ms > 0
    assert out.bytes_received > 0
    assert out.protocol == "open_ai"
    assert out.shape == "stream"


@pytest.mark.asyncio
async def test_run_one_request_records_client_side_failure():
    transport = httpx.MockTransport(lambda r: (_ for _ in ()).throw(
        httpx.ConnectError("simulated")
    ))
    client = httpx.AsyncClient(transport=transport, timeout=1.0,
                               base_url="http://gateway.local")
    spec = RequestSpec(
        protocol="open_ai", shape="non_stream", cross_protocol=False,
        payload_size_bucket="small",
        user_id="u-1", api_key_id="ak-1", api_key_plaintext="sk-1",
    )
    handles = SoakSeedHandles(
        users=[("u-1", "ak-1", "sk-1")],
        groups_by_kind={"open_ai": "g", "anthropic": "g", "gemini": "g", "cross": "g"},
        models_by_kind={"open_ai": SOAK_MODEL_OPENAI,
                        "anthropic": "x", "gemini": "y"},
    )
    out = await _run_one_request(client, spec, handles, timeout_s=1)
    assert out.failed_client_side is True
    assert out.status is None
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_driver.py -v
```

Expected: `_run_one_request` not defined.

- [ ] **Step 3: Implement `_run_one_request`**

Append to `tests/e2e/client/soak/driver.py`:

```python
import asyncio
import json
import re
import threading
import time
from queue import SimpleQueue, Empty
from typing import Iterable

import httpx

from soak.seed import SoakSeedHandles
from soak.workload import RequestSpec, build_request


# Best-effort parser for the error_code embedded in the gateway's
# OpenAI/Anthropic/Gemini error envelopes. All three families wrap the
# code under `{"error": {"code": "..."}}` per
# crates/gw-codec/src/*/codec.rs::render_error. SSE error frames also
# embed the same envelope as a `data: ` line — we grep for the first one.
_ERR_CODE_RE = re.compile(rb'"code"\s*:\s*"(GW_[A-Z_]+)"')


def _extract_error_code(body_bytes: bytes) -> str | None:
    m = _ERR_CODE_RE.search(body_bytes)
    return m.group(1).decode() if m else None


async def _run_one_request(
    client: httpx.AsyncClient,
    spec: RequestSpec,
    handles: SoakSeedHandles,
    timeout_s: float,
) -> RequestOutcome:
    req = build_request(spec, handles)
    t0 = time.perf_counter()
    status: int | None = None
    error_code: str | None = None
    bytes_received = 0
    failed = False
    try:
        async with client.stream(
            req["method"], req["path"],
            headers=req["headers"],
            json=req["json"],
            timeout=timeout_s,
        ) as resp:
            status = resp.status_code
            buf = bytearray()
            async for chunk in resp.aiter_bytes():
                bytes_received += len(chunk)
                buf.extend(chunk)
                if bytes_received > 64 * 1024:
                    # Cap memory per response — we only need the head to
                    # find the error_code envelope. Real content can be
                    # streamed indefinitely without OOM.
                    break
            error_code = _extract_error_code(bytes(buf))
    except (httpx.HTTPError, asyncio.TimeoutError):
        failed = True

    latency_ms = (time.perf_counter() - t0) * 1000.0
    return RequestOutcome(
        protocol=spec.protocol,
        shape=spec.shape,
        cross_protocol=spec.cross_protocol,
        status=status,
        error_code=error_code,
        latency_ms=latency_ms,
        bytes_received=bytes_received,
        failed_client_side=failed,
    )


def _thread_loop(
    thread_idx: int,
    cfg: SoakConfig,
    handles: SoakSeedHandles,
    in_queue: SimpleQueue,
    stop_event: threading.Event,
    outcomes: list[RequestOutcome],
    outcomes_lock: threading.Lock,
) -> None:
    """Run one OS thread. Owns one asyncio loop + one AsyncClient."""
    async def runner() -> None:
        limits = httpx.Limits(
            max_connections=cfg.max_inflight_per_thread * 4,
            max_keepalive_connections=cfg.max_inflight_per_thread * 2,
        )
        async with httpx.AsyncClient(
            base_url=cfg.gateway_url,
            limits=limits,
            timeout=cfg.per_request_timeout_s,
        ) as client:
            sem = asyncio.Semaphore(cfg.max_inflight_per_thread)
            inflight: set[asyncio.Task] = set()

            async def fire(spec: RequestSpec) -> None:
                async with sem:
                    out = await _run_one_request(
                        client, spec, handles, cfg.per_request_timeout_s
                    )
                    with outcomes_lock:
                        outcomes.append(out)

            # Drain the queue until stop_event AND queue is empty.
            while True:
                try:
                    spec = in_queue.get(timeout=0.05)
                except Empty:
                    if stop_event.is_set() and not inflight:
                        break
                    # Reap finished tasks to bound the set size.
                    inflight = {t for t in inflight if not t.done()}
                    await asyncio.sleep(0)
                    continue
                task = asyncio.create_task(fire(spec))
                inflight.add(task)
                inflight = {t for t in inflight if not t.done()}

            # Final grace: await all in-flight up to drain_grace_s.
            if inflight:
                try:
                    await asyncio.wait_for(
                        asyncio.gather(*inflight, return_exceptions=True),
                        timeout=cfg.drain_grace_s,
                    )
                except asyncio.TimeoutError:
                    pass

    asyncio.run(runner())
```

- [ ] **Step 4: Run the tests**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_driver.py -v
```

Expected: 5 PASS (3 from Task 11 + 2 new).

- [ ] **Step 5: Commit**

```bash
git add tests/e2e/client/soak/driver.py tests/e2e/client/soak/test_driver.py
git commit -m "soak(py): per-thread async runner + error_code regex

_run_one_request streams the response, caps at 64KiB head capture,
greps the GW_* code envelope. _thread_loop drains the inbound queue
into its own AsyncClient up to max_inflight_per_thread and respects
the drain_grace_s ceiling on shutdown."
```

---

### Task 13: Rate emitter + livez probe + `run()` aggregation

**Files:**
- Modify: `tests/e2e/client/soak/driver.py` (add `SoakDriver.run`, livez probe)
- Test: append to `tests/e2e/client/soak/test_driver.py`

Per spec §6.1 + §7.1.1. Rate emitter uses absolute monotonic deadlines (not naive `sleep(1/rps)` accumulation drift). `/livez` probe runs every 10 s on a background thread.

- [ ] **Step 1: Write the failing test (smoke-shaped run against a MockTransport gateway)**

Append to `tests/e2e/client/soak/test_driver.py`:

```python
import socket

import httpx
import pytest


def test_soak_driver_runs_against_loopback(monkeypatch):
    # Run a *very* short soak against a real loopback http server (httpx
    # MockTransport doesn't compose with multi-thread asyncio nicely).
    # 5 threads × 100 rps × 1s = ~500 reqs.
    import http.server
    import threading as _t

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):  # noqa: N802
            n = int(self.headers.get("content-length", 0))
            self.rfile.read(n)
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("transfer-encoding", "chunked")
            self.end_headers()
            self.wfile.write(b"1f\r\ndata: {\"choices\":[{\"delta\":{}}]}\n\n\r\n")
            self.wfile.write(b"a\r\ndata: [DONE]\n\n\r\n")
            self.wfile.write(b"0\r\n\r\n")
        def log_message(self, *a, **k):  # silence
            return
        # livez/readyz
        def do_GET(self):  # noqa: N802
            self.send_response(200)
            self.send_header("content-type", "text/plain")
            self.end_headers()
            self.wfile.write(b"ok")

    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    th = _t.Thread(target=srv.serve_forever, daemon=True)
    th.start()
    try:
        from soak.driver import SoakConfig, SoakDriver
        from soak.seed import SoakSeedHandles, SOAK_MODEL_OPENAI

        cfg = SoakConfig(
            duration_s=1,
            target_rps=100,
            n_threads=2,
            max_inflight_per_thread=16,
            per_request_timeout_s=5,
            drain_grace_s=2,
            n_users=2,
            gateway_url=f"http://127.0.0.1:{port}",
            livez_url=f"http://127.0.0.1:{port}/livez",
            readyz_url=f"http://127.0.0.1:{port}/readyz",
        )
        handles = SoakSeedHandles(
            users=[("u-1", "ak-1", "sk-1"), ("u-2", "ak-2", "sk-2")],
            groups_by_kind={"open_ai": "g", "anthropic": "g",
                            "gemini": "g", "cross": "g"},
            models_by_kind={"open_ai": SOAK_MODEL_OPENAI,
                            "anthropic": "x", "gemini": "y"},
        )
        report = SoakDriver(cfg, handles).run()
        # Loose lower bound — CI may not sustain 100 rps exactly.
        assert report.requests_completed >= 30, report
        assert report.duration_ms >= 1000
        assert report.by_status.get(200, 0) >= 30
        assert report.livez_probes >= 1
        assert report.livez_non_200 == 0
    finally:
        srv.shutdown()
        th.join(timeout=2)
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_driver.py::test_soak_driver_runs_against_loopback -v
```

Expected: `SoakDriver` not defined.

- [ ] **Step 3: Implement `SoakDriver`**

Append to `tests/e2e/client/soak/driver.py`:

```python
import random
import statistics
import urllib.request
from concurrent.futures import ThreadPoolExecutor

from soak.workload import WorkloadMix, sample_request_spec


class SoakDriver:
    def __init__(
        self,
        cfg: SoakConfig,
        handles: SoakSeedHandles,
        mix: WorkloadMix | None = None,
        rng_seed: int = 0xC0FFEE,
    ) -> None:
        self.cfg = cfg
        self.handles = handles
        self.mix = mix or WorkloadMix.default()
        self.rng_seed = rng_seed

    # ── livez/readyz background probe ────────────────────────────────────

    def _probe(self, url: str, period_s: float, stop: threading.Event,
               counter: dict) -> None:
        while not stop.is_set():
            try:
                r = urllib.request.urlopen(url, timeout=2)  # noqa: S310
                counter["probes"] += 1
                if r.status != 200:
                    counter["non_200"] += 1
            except Exception:
                counter["probes"] += 1
                counter["non_200"] += 1
            stop.wait(period_s)

    # ── main entry ───────────────────────────────────────────────────────

    def run(self) -> SoakDriverReport:
        cfg = self.cfg
        target_count = cfg.target_count
        rng = random.Random(self.rng_seed)

        outcomes: list[RequestOutcome] = []
        outcomes_lock = threading.Lock()
        queues: list[SimpleQueue] = [SimpleQueue() for _ in range(cfg.n_threads)]
        stop_event = threading.Event()

        # Spawn worker threads.
        with ThreadPoolExecutor(max_workers=cfg.n_threads + 2) as pool:
            for ti in range(cfg.n_threads):
                pool.submit(
                    _thread_loop, ti, cfg, self.handles, queues[ti],
                    stop_event, outcomes, outcomes_lock,
                )

            livez_counter = {"probes": 0, "non_200": 0}
            readyz_counter = {"probes": 0, "non_200": 0}
            probe_stop = threading.Event()
            pool.submit(self._probe, cfg.livez_url, 10.0, probe_stop, livez_counter)
            pool.submit(self._probe, cfg.readyz_url, 10.0, probe_stop, readyz_counter)

            t0 = time.perf_counter()
            deadline = t0 + cfg.duration_s
            tick_interval = 1.0 / cfg.target_rps
            sent = 0

            for i in range(target_count):
                # Absolute monotonic schedule — no naive sleep accumulation.
                scheduled = t0 + i * tick_interval
                now = time.perf_counter()
                if scheduled > now:
                    time.sleep(scheduled - now)
                if time.perf_counter() >= deadline:
                    break
                spec = sample_request_spec(self.mix, self.handles, rng)
                queues[i % cfg.n_threads].put(spec)
                sent += 1

            duration_ms = int((time.perf_counter() - t0) * 1000)
            stop_event.set()
            probe_stop.set()
            # Workers drain on stop_event + final grace inside _thread_loop.

        # Aggregate.
        completed = len(outcomes)
        report = SoakDriverReport.empty(target_count=target_count)
        report.duration_ms = duration_ms
        report.requests_sent = sent
        report.requests_completed = completed
        report.livez_probes = livez_counter["probes"]
        report.livez_non_200 = livez_counter["non_200"]
        report.readyz_probes = readyz_counter["probes"]
        report.readyz_non_200 = readyz_counter["non_200"]

        latencies: list[float] = []
        for o in outcomes:
            if o.failed_client_side:
                report.requests_failed_client_side += 1
            else:
                report.bytes_received_total += o.bytes_received
                latencies.append(o.latency_ms)
                if o.status is not None:
                    report.by_status[o.status] = report.by_status.get(o.status, 0) + 1
                if o.error_code:
                    report.by_error_code[o.error_code] = report.by_error_code.get(o.error_code, 0) + 1
            report.by_protocol[o.protocol] = report.by_protocol.get(o.protocol, 0) + 1
            report.by_shape[o.shape] = report.by_shape.get(o.shape, 0) + 1
            report.by_cross_proto[o.cross_protocol] = report.by_cross_proto.get(o.cross_protocol, 0) + 1

        if latencies:
            latencies.sort()
            report.latency_ms_p50 = statistics.median(latencies)
            report.latency_ms_p90 = latencies[int(len(latencies) * 0.90)]
            report.latency_ms_p99 = latencies[int(len(latencies) * 0.99)]
            report.latency_ms_max = latencies[-1]

        return report
```

- [ ] **Step 4: Run the test**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_driver.py::test_soak_driver_runs_against_loopback -v
```

Expected: PASS. Tolerances are loose (≥30 of ~100 expected at 100 rps) to survive CI noise.

- [ ] **Step 5: Commit**

```bash
git add tests/e2e/client/soak/driver.py tests/e2e/client/soak/test_driver.py
git commit -m "soak(py): SoakDriver.run() + livez/readyz probes

Absolute-monotonic rate emitter (no sleep-drift accumulation),
n_threads worker queues for round-robin dispatch, livez/readyz
probed every 10s on background threads. Post-run aggregation
computes by_status/by_error_code/by_protocol/by_shape/by_cross
spectra + p50/p90/p99/max latencies for the driver report."
```

---

## Phase 5 — Python invariants & report

### Task 14: `log_scan.py` — panic-class log grep

**Files:**
- Create: `tests/e2e/client/soak/log_scan.py`
- Test: `tests/e2e/client/soak/test_log_scan.py`

Per spec §7.3 + C3 (no whitelist). The scan is narrow by design.

- [ ] **Step 1: Write the failing test**

Create `tests/e2e/client/soak/test_log_scan.py`:

```python
from soak.log_scan import PANIC_PATTERN, scan_panic_lines


SAMPLE_GOOD = """\
2026-05-22T14:30:00Z INFO  gateway: started
2026-05-22T14:30:01Z WARN  gw_health: key vk-1 cooled down for 30s
2026-05-22T14:30:02Z ERROR gw_routing: no available channel for group g
2026-05-22T14:30:03Z INFO  gateway: served 1000 requests
"""

SAMPLE_BAD = """\
2026-05-22T14:30:42.123Z gateway thread 'tokio-runtime-worker' panicked at 'index out of bounds: the len is 4 but the index is 99', src/foo.rs:42
note: run with `RUST_BACKTRACE=1` for backtrace
2026-05-22T14:30:43.000Z gateway assertion failed: x == y
"""


def test_scan_panic_lines_passes_normal_warn_error():
    matched = scan_panic_lines(SAMPLE_GOOD)
    assert matched == []


def test_scan_panic_lines_catches_panic_and_assertion():
    matched = scan_panic_lines(SAMPLE_BAD)
    assert len(matched) == 3
    assert any("panicked at" in m for m in matched)
    assert any("RUST_BACKTRACE" in m for m in matched)
    assert any("assertion failed" in m for m in matched)


def test_scan_panic_pattern_is_exposed_for_reporting():
    # Spec §7.1.1 — the report includes the exact pattern that was matched,
    # so consumers can see what was searched.
    assert "panicked at" in PANIC_PATTERN
    assert "RUST_BACKTRACE" in PANIC_PATTERN
    assert "assertion failed" in PANIC_PATTERN
    assert "fatal" in PANIC_PATTERN.lower()
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_log_scan.py -v
```

Expected: ImportError.

- [ ] **Step 3: Implement `log_scan.py`**

Create `tests/e2e/client/soak/log_scan.py`:

```python
"""Panic-class log scan — spec §7.3 + C3.

Deliberately narrow: WARN/ERROR are NOT scanned (the gateway emits them
on normal cooldown / quota / health paths). The patterns here are
100%-bug signals — any match is a real bug to file, with no whitelist
escape hatch.
"""
from __future__ import annotations

import re

# Single regex with alternation; flags=re.IGNORECASE handles `fatal` and
# the panic-thread variants. `\b` on `fatal` avoids matching `fatality` /
# `fatalistic` etc. in arbitrary log payloads.
PANIC_PATTERN = (
    r"panicked at"
    r"|RUST_BACKTRACE"
    r"|thread '.*' panicked"
    r"|assertion failed"
    r"|\bfatal\b"
)

_RE = re.compile(PANIC_PATTERN, re.IGNORECASE)


def scan_panic_lines(text: str) -> list[str]:
    """Return every line in `text` that matches the panic pattern."""
    return [line for line in text.splitlines() if _RE.search(line)]
```

- [ ] **Step 4: Run + commit**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_log_scan.py -v
```

Expected: 3 PASS.

```bash
git add tests/e2e/client/soak/log_scan.py tests/e2e/client/soak/test_log_scan.py
git commit -m "soak(py): log_scan.py — panic-class signal grep

PANIC_PATTERN: panic / backtrace / 'thread ... panicked' / assertion
failed / \bfatal\b. WARN/ERROR explicitly NOT scanned (spec C3).
Any match is a bug. The pattern is exposed so the report can include
the exact search string in raw.scan_pattern (spec §7.1.1)."
```

---

### Task 15: `invariants.py` — process_alive group

**Files:**
- Create: `tests/e2e/client/soak/invariants.py` (scaffolding + group 1)
- Test: `tests/e2e/client/soak/test_invariants.py`

Per spec §7.1.1. Each invariant returns a `dict` matching the report JSON leaf shape (path/kind/passed/source/raw).

- [ ] **Step 1: Write the failing test**

Create `tests/e2e/client/soak/test_invariants.py`:

```python
from soak.driver import SoakDriverReport
from soak.invariants import (
    process_alive_no_panic,
    process_alive_livez_throughout,
    process_alive_readyz_throughout,
    process_alive_no_container_restart,
)


def test_no_panic_pass_on_clean_logs():
    leaf = process_alive_no_panic("2026-05-22 INFO gateway: started")
    assert leaf["passed"] is True
    assert leaf["raw"]["match_count"] == 0
    assert leaf["raw"]["matched_lines"] == []
    assert leaf["kind"] == "hard"
    assert leaf["source"] == "docker.compose_logs.gateway"


def test_no_panic_fail_on_panic_line():
    leaf = process_alive_no_panic(
        "INFO gateway: started\nthread 'tokio' panicked at boom"
    )
    assert leaf["passed"] is False
    assert leaf["raw"]["match_count"] == 1


def test_livez_throughout_pass():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.livez_probes = 6
    rpt.livez_non_200 = 0
    leaf = process_alive_livez_throughout(rpt)
    assert leaf["passed"] is True
    assert leaf["raw"]["non_200_count"] == 0


def test_livez_throughout_fail():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.livez_probes = 6
    rpt.livez_non_200 = 1
    leaf = process_alive_livez_throughout(rpt)
    assert leaf["passed"] is False


def test_no_container_restart_pass():
    leaf = process_alive_no_container_restart(
        before={"RestartCount": 0, "State": {"Status": "running"}},
        after={"RestartCount": 0, "State": {"Status": "running"}},
    )
    assert leaf["passed"] is True


def test_no_container_restart_fail_on_restart_increment():
    leaf = process_alive_no_container_restart(
        before={"RestartCount": 0, "State": {"Status": "running"}},
        after={"RestartCount": 1, "State": {"Status": "running"}},
    )
    assert leaf["passed"] is False
    assert leaf["raw"]["restart_count_delta"] == 1
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_invariants.py -v
```

Expected: ImportError.

- [ ] **Step 3: Implement `invariants.py` scaffolding + group 1**

Create `tests/e2e/client/soak/invariants.py`:

```python
"""Invariant evaluators for the soak harness — spec §7.

Each function returns a dict matching the report JSON leaf shape:

    {
      "path":   "results.invariants.<group>.<name>",
      "kind":   "hard" | "soft",
      "passed": bool | None,        # None means "data unavailable"
      "source": "<concrete endpoint/method>",
      "raw":    { ...evidence... },
      "threshold": "<optional, when not obvious from raw>",
    }

Per spec C4, every leaf has a non-null `source` and `raw` carries the
raw observation. Missing data is `passed: None` with `raw.reason`.
"""
from __future__ import annotations

from soak.driver import SoakDriverReport
from soak.log_scan import PANIC_PATTERN, scan_panic_lines


def _leaf(path: str, *, kind: str, passed: bool | None, source: str,
          raw: dict, threshold: str | None = None) -> dict:
    out = {
        "path": path,
        "kind": kind,
        "passed": passed,
        "source": source,
        "raw": raw,
    }
    if threshold is not None:
        out["threshold"] = threshold
    return out


# ── Group 1: process_alive (§7.1.1) ──────────────────────────────────────

def process_alive_no_panic(gateway_log_text: str) -> dict:
    matched = scan_panic_lines(gateway_log_text)
    return _leaf(
        "results.invariants.process_alive.no_panic",
        kind="hard",
        passed=(len(matched) == 0),
        source="docker.compose_logs.gateway",
        raw={
            "scan_pattern": PANIC_PATTERN,
            "matched_lines": matched,
            "match_count": len(matched),
        },
    )


def process_alive_livez_throughout(rpt: SoakDriverReport) -> dict:
    return _leaf(
        "results.invariants.process_alive.livez_200_throughout",
        kind="hard",
        passed=(rpt.livez_non_200 == 0 and rpt.livez_probes > 0),
        source="driver.livez_probe",
        raw={"probes": rpt.livez_probes, "non_200_count": rpt.livez_non_200},
    )


def process_alive_readyz_throughout(rpt: SoakDriverReport) -> dict:
    return _leaf(
        "results.invariants.process_alive.readyz_200_throughout",
        kind="hard",
        passed=(rpt.readyz_non_200 == 0 and rpt.readyz_probes > 0),
        source="driver.readyz_probe",
        raw={"probes": rpt.readyz_probes, "non_200_count": rpt.readyz_non_200},
    )


def process_alive_no_container_restart(*, before: dict, after: dict) -> dict:
    rc_before = int(before.get("RestartCount", 0))
    rc_after = int(after.get("RestartCount", 0))
    state_after = (after.get("State") or {}).get("Status", "unknown")
    passed = (rc_after - rc_before == 0) and state_after == "running"
    return _leaf(
        "results.invariants.process_alive.no_container_restart",
        kind="hard",
        passed=passed,
        source="docker.compose_ps.gateway",
        raw={
            "restart_count_before": rc_before,
            "restart_count_after": rc_after,
            "restart_count_delta": rc_after - rc_before,
            "state_after": state_after,
        },
    )
```

- [ ] **Step 4: Run + commit**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_invariants.py -v
```

Expected: 6 PASS.

```bash
git add tests/e2e/client/soak/invariants.py tests/e2e/client/soak/test_invariants.py
git commit -m "soak(py): invariants — process_alive group (§7.1.1)

no_panic + livez_throughout + readyz_throughout +
no_container_restart. Each returns the canonical leaf dict shape
(path/kind/passed/source/raw) used by the report writer."
```

---

### Task 16: `invariants.py` — account_consistency group

**Files:**
- Modify: `tests/e2e/client/soak/invariants.py` (append group 2)
- Test: append to `tests/e2e/client/soak/test_invariants.py`

Per spec §7.1.2 — the strict-equality records vs upstream-hits check, monotonic balance decrease, and drain-quiescence gate.

- [ ] **Step 1: Write the failing test**

Append to `tests/e2e/client/soak/test_invariants.py`:

```python
from soak.invariants import (
    account_records_equals_upstream_hits,
    account_balance_monotonic_decreasing,
    account_drain_quiescence_reached,
)


def test_records_equals_hits_pass():
    leaf = account_records_equals_upstream_hits(
        window={
            "redis_streams": {
                "usage_records_delta": 50001,
                "usage_records_dlq_delta": 0,
            },
            "drain_observation_series": [49991, 50001, 50001, 50001, 50001,
                                          50001, 50001, 50001, 50001, 50001],
        },
        hits={"routes_hit_sum": 50001, "refused_hit_sum": 0, "total": 50001},
    )
    assert leaf["passed"] is True
    assert leaf["raw"]["diff"] == 0
    assert leaf["threshold"] == "=="


def test_records_equals_hits_fail_diff_1():
    leaf = account_records_equals_upstream_hits(
        window={
            "redis_streams": {
                "usage_records_delta": 49997,
                "usage_records_dlq_delta": 3,
            },
            "drain_observation_series": [49997, 50000, 50000, 50000, 50000,
                                          50000, 50000, 50000, 50000, 50000],
        },
        hits={"routes_hit_sum": 49998, "refused_hit_sum": 3, "total": 50001},
    )
    assert leaf["passed"] is False
    assert leaf["raw"]["diff"] == 1


def test_balance_monotonic_pass():
    leaf = account_balance_monotonic_decreasing(
        window={"balance_decrement_micros": {"u-1": 1000, "u-2": 0, "u-3": 50}},
    )
    assert leaf["passed"] is True


def test_balance_monotonic_fail_on_negative_decrement():
    leaf = account_balance_monotonic_decreasing(
        window={"balance_decrement_micros": {"u-1": 1000, "u-2": -50}},
    )
    assert leaf["passed"] is False
    assert leaf["raw"]["negative_users"] == ["u-2"]


def test_drain_quiescence_pass():
    leaf = account_drain_quiescence_reached(window={"drain_quiescent": True})
    assert leaf["passed"] is True


def test_drain_quiescence_fail():
    leaf = account_drain_quiescence_reached(window={"drain_quiescent": False})
    assert leaf["passed"] is False
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_invariants.py -k account -v
```

Expected: ImportError.

- [ ] **Step 3: Implement group 2**

Append to `tests/e2e/client/soak/invariants.py`:

```python
# ── Group 2: account_consistency (§7.1.2) ────────────────────────────────

def account_records_equals_upstream_hits(*, window: dict, hits: dict) -> dict:
    rs = window.get("redis_streams") or {}
    records = int(rs.get("usage_records_delta", 0))
    dlq = int(rs.get("usage_records_dlq_delta", 0))
    routes_sum = int(hits.get("routes_hit_sum", 0))
    refused_sum = int(hits.get("refused_hit_sum", 0))
    total = int(hits.get("total", routes_sum + refused_sum))
    diff = abs((records + dlq) - total)
    return _leaf(
        "results.invariants.account_consistency.records_equals_upstream_hits",
        kind="hard",
        passed=(diff == 0),
        source="fake_web:/soak/window/report + fake_upstream:/admin/hits-total",
        raw={
            "records_delta": records,
            "dlq_delta": dlq,
            "upstream_routes_hit_sum": routes_sum,
            "upstream_refused_hit_sum": refused_sum,
            "upstream_hits_total": total,
            "diff": diff,
            "drain_observation_series": window.get("drain_observation_series", []),
        },
        threshold="==",
    )


def account_balance_monotonic_decreasing(*, window: dict) -> dict:
    decs: dict[str, int] = window.get("balance_decrement_micros") or {}
    negative = [u for u, v in decs.items() if v < 0]
    return _leaf(
        "results.invariants.account_consistency.balance_monotonic_decreasing",
        kind="hard",
        passed=(len(negative) == 0),
        source="fake_web:/soak/window/report.balance_decrement_micros",
        raw={
            "user_count": len(decs),
            "negative_users": negative,
        },
    )


def account_drain_quiescence_reached(*, window: dict) -> dict:
    quiescent = bool(window.get("drain_quiescent", False))
    return _leaf(
        "results.invariants.account_consistency.drain_quiescence_reached",
        kind="hard",
        passed=quiescent,
        source="fake_web:/soak/window/report.drain_quiescent",
        raw={
            "drain_quiescent": quiescent,
            "drain_observation_series": window.get("drain_observation_series", []),
        },
    )
```

- [ ] **Step 4: Run + commit**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_invariants.py -v
```

Expected: 12 PASS (6 from Task 15 + 6 new).

```bash
git add tests/e2e/client/soak/invariants.py tests/e2e/client/soak/test_invariants.py
git commit -m "soak(py): invariants — account_consistency group (§7.1.2)

Strict-equality records+dlq == upstream_hits_total (spec C5 enables
this via fixed_order + no fallback in seed). Balance must be
monotonically decreasing per user. Drain must hit quiescence within
60s ceiling."
```

---

### Task 17: `invariants.py` — resource_release, error_code_compliance, chaos_causal, min_completion

**Files:**
- Modify: `tests/e2e/client/soak/invariants.py` (append groups 3-6)
- Test: append to `tests/e2e/client/soak/test_invariants.py`

Per spec §7.1.3, §7.1.4, §7.1.5, §7.1.6.

- [ ] **Step 1: Write the failing tests**

Append to `tests/e2e/client/soak/test_invariants.py`:

```python
from soak.invariants import (
    resource_all_in_flight_gauges_zero,
    resource_rss_growth_under_threshold,
    error_code_compliance_all_in_catalog,
    chaos_causal_per_variant,
    min_completion_at_least_half,
    VARIANT_ERROR_CODE_MAP,
)
from soak.driver import SoakDriverReport


def test_all_in_flight_gauges_zero_pass():
    metrics_text = """\
gw_routing_inflight_per_key{key_id="vk-1"} 0
gw_proxy_streams_in_flight 0
"""
    leaf = resource_all_in_flight_gauges_zero(metrics_text)
    assert leaf["passed"] is True
    assert leaf["raw"]["non_zero_gauges"] == {}


def test_all_in_flight_gauges_zero_fail():
    metrics_text = """\
gw_routing_inflight_per_key{key_id="vk-1"} 2
gw_proxy_streams_in_flight 0
"""
    leaf = resource_all_in_flight_gauges_zero(metrics_text)
    assert leaf["passed"] is False
    assert leaf["raw"]["non_zero_gauges"][
        'gw_routing_inflight_per_key{key_id="vk-1"}'
    ] == 2.0


def test_rss_growth_pass():
    leaf = resource_rss_growth_under_threshold(
        before_bytes=100 * 1024 * 1024,
        after_bytes=180 * 1024 * 1024,
        threshold_mib=100,
    )
    assert leaf["passed"] is True


def test_rss_growth_fail():
    leaf = resource_rss_growth_under_threshold(
        before_bytes=100 * 1024 * 1024,
        after_bytes=300 * 1024 * 1024,
        threshold_mib=100,
    )
    assert leaf["passed"] is False


def test_error_code_compliance_pass():
    leaf = error_code_compliance_all_in_catalog(
        observed={"GW_UPSTREAM_TIMEOUT", "GW_UPSTREAM_5XX"},
        catalog={"GW_UPSTREAM_TIMEOUT", "GW_UPSTREAM_5XX", "GW_AUTH_INVALID_APIKEY"},
        catalog_source="codes.rs@abc1234",
    )
    assert leaf["passed"] is True
    assert leaf["raw"]["unknown"] == []


def test_error_code_compliance_fail():
    leaf = error_code_compliance_all_in_catalog(
        observed={"GW_UPSTREAM_TIMEOUT", "GW_MADE_UP_CODE"},
        catalog={"GW_UPSTREAM_TIMEOUT"},
        catalog_source="codes.rs@abc1234",
    )
    assert leaf["passed"] is False
    assert "GW_MADE_UP_CODE" in leaf["raw"]["unknown"]


def test_variant_error_code_map_codes_exist_in_typical_catalog():
    typical = {
        "GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_CANCELLED",
        "GW_UPSTREAM_TIMEOUT", "GW_UPSTREAM_OVERLOADED", "GW_UPSTREAM_5XX",
    }
    for variant_kind, codes in VARIANT_ERROR_CODE_MAP.items():
        assert codes, f"variant {variant_kind} has empty code set"
        # Every code in the map MUST be one of the GW_UPSTREAM_* codes
        # that classify_default emits — see spec Appendix A.
        for c in codes:
            assert c in typical, f"{variant_kind}: {c} not in typical catalog"


def test_chaos_causal_pass():
    chaos_stats = {
        "enabled": True,
        "variants": [
            {"variant_index": 0, "kind": "sse", "hits": 100, "weight": 80},
            {"variant_index": 1, "kind": "tcp_reset_mid_body",
             "hits": 10, "weight": 5},
        ],
    }
    by_error_code = {"GW_UPSTREAM_CONNECT_FAILED": 9, "GW_UPSTREAM_CANCELLED": 0}
    leaf = chaos_causal_per_variant(chaos_stats, by_error_code, min_ratio=0.7)
    per = leaf["raw"]["per_variant"]
    assert per["tcp_reset_mid_body"]["passed"] is True
    assert per["tcp_reset_mid_body"]["matched_error_count"] == 9
    assert leaf["passed"] is True


def test_chaos_causal_fail_when_no_mapped_codes():
    chaos_stats = {
        "enabled": True,
        "variants": [
            {"variant_index": 0, "kind": "sse", "hits": 100, "weight": 80},
            {"variant_index": 1, "kind": "tcp_reset_mid_body",
             "hits": 100, "weight": 5},
        ],
    }
    by_error_code = {"GW_UPSTREAM_CONNECT_FAILED": 0, "GW_UPSTREAM_CANCELLED": 0}
    leaf = chaos_causal_per_variant(chaos_stats, by_error_code, min_ratio=0.7)
    assert leaf["raw"]["per_variant"]["tcp_reset_mid_body"]["passed"] is False
    assert leaf["passed"] is False


def test_min_completion_pass():
    rpt = SoakDriverReport.empty(target_count=49980)
    rpt.requests_completed = 30000
    leaf = min_completion_at_least_half(rpt)
    assert leaf["passed"] is True


def test_min_completion_fail():
    rpt = SoakDriverReport.empty(target_count=49980)
    rpt.requests_completed = 10000
    leaf = min_completion_at_least_half(rpt)
    assert leaf["passed"] is False
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_invariants.py -k "in_flight or rss or error_code or chaos_causal or min_completion or variant_error_code" -v
```

Expected: ImportError on the new names.

- [ ] **Step 3: Implement groups 3-6**

Append to `tests/e2e/client/soak/invariants.py`:

```python
import re


# ── Group 3: resource_release (§7.1.3) ───────────────────────────────────

_INFLIGHT_RE = re.compile(
    r"^(?P<series>gw_[a-zA-Z0-9_]*in_flight[a-zA-Z0-9_]*(\{[^}]*\})?)\s+(?P<v>[-+0-9eE.]+)\s*$",
    re.MULTILINE,
)


def resource_all_in_flight_gauges_zero(metrics_text: str) -> dict:
    non_zero: dict[str, float] = {}
    for m in _INFLIGHT_RE.finditer(metrics_text):
        try:
            v = float(m.group("v"))
        except ValueError:
            continue
        if v != 0:
            non_zero[m.group("series")] = v
    return _leaf(
        "results.invariants.resource_release.all_in_flight_gauges_zero",
        kind="hard",
        passed=(len(non_zero) == 0),
        source="gateway:/metrics",
        raw={
            "scan_pattern": "^gw_*in_flight* series",
            "non_zero_gauges": non_zero,
        },
    )


def resource_rss_growth_under_threshold(
    *,
    before_bytes: int,
    after_bytes: int,
    threshold_mib: int = 100,
) -> dict:
    growth_bytes = max(0, after_bytes - before_bytes)
    growth_mib = growth_bytes / (1024 * 1024)
    return _leaf(
        "results.invariants.resource_release.rss_growth_under_threshold",
        kind="hard",
        passed=(growth_mib < threshold_mib),
        source="docker.stats.gateway",
        raw={
            "rss_before_bytes": before_bytes,
            "rss_after_bytes": after_bytes,
            "growth_bytes": growth_bytes,
            "growth_mib": round(growth_mib, 2),
            "threshold_mib": threshold_mib,
        },
    )


# ── Group 4: error_code_compliance (§7.1.4) ──────────────────────────────

def error_code_compliance_all_in_catalog(
    *,
    observed: set[str],
    catalog: set[str],
    catalog_source: str,
) -> dict:
    unknown = sorted(observed - catalog)
    return _leaf(
        "results.invariants.error_code_compliance.all_codes_in_catalog",
        kind="hard",
        passed=(len(unknown) == 0),
        source=f"driver.by_error_code ∪ fake_web.detailed_log_spectrum vs {catalog_source}",
        raw={
            "catalog_source": catalog_source,
            "observed": sorted(observed),
            "unknown": unknown,
        },
    )


# ── Group 5: chaos_causal_reconciliation (§7.1.5) ────────────────────────

# Spec Appendix A — pinned variant→error_code mapping.
VARIANT_ERROR_CODE_MAP: dict[str, list[str]] = {
    "tcp_reset_after_headers": ["GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_CANCELLED"],
    "tcp_reset_mid_body":      ["GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_CANCELLED"],
    "connection_refused_first_n": ["GW_UPSTREAM_CONNECT_FAILED"],
    "slow_body_byte_per_second":  ["GW_UPSTREAM_TIMEOUT"],
    "partial_sse_event": ["GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_TIMEOUT",
                          "GW_UPSTREAM_CANCELLED"],
    "status_with_retry_after": ["GW_UPSTREAM_OVERLOADED"],
    "static":                  ["GW_UPSTREAM_5XX"],
}


def chaos_causal_per_variant(
    chaos_stats: dict,
    by_error_code: dict[str, int],
    *,
    min_ratio: float = 0.7,
) -> dict:
    """Spec §7.1.5 — per-variant `E ≥ floor(H × min_ratio)`.

    The `sse` variant is skipped (the happy path is not a fault).
    Variants with `hits == 0` are also skipped (no causality to check).
    """
    per_variant: dict[str, dict] = {}
    overall_pass = True
    for v in chaos_stats.get("variants", []):
        kind = v.get("kind", "")
        hits = int(v.get("hits", 0))
        if kind == "sse" or hits == 0:
            continue
        mapped = VARIANT_ERROR_CODE_MAP.get(kind, [])
        matched = sum(by_error_code.get(c, 0) for c in mapped)
        required = int(hits * min_ratio)
        passed = matched >= required
        per_variant[kind] = {
            "variant_index": v.get("variant_index"),
            "variant_hits": hits,
            "mapped_error_codes": mapped,
            "matched_error_count": matched,
            "ratio": round(matched / hits, 3) if hits else None,
            "min_ratio": min_ratio,
            "passed": passed,
        }
        if not passed:
            overall_pass = False
    return _leaf(
        "results.invariants.chaos_causal_reconciliation.summary",
        kind="hard",
        passed=overall_pass if per_variant else True,
        source="fake_upstream:/admin/chaos-stats + driver.by_error_code",
        raw={"per_variant": per_variant},
    )


# ── Group 6: min_completion (§7.1.6) ─────────────────────────────────────

def min_completion_at_least_half(rpt: SoakDriverReport) -> dict:
    threshold = rpt.target_count * 0.5
    return _leaf(
        "results.invariants.min_completion.at_least_half_completed",
        kind="hard",
        passed=(rpt.requests_completed >= threshold),
        source="driver.requests_completed",
        raw={
            "requests_completed": rpt.requests_completed,
            "target_count": rpt.target_count,
            "threshold_count": int(threshold),
        },
    )
```

- [ ] **Step 4: Run + commit**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_invariants.py -v
```

Expected: 22 PASS (12 prior + 10 new).

```bash
git add tests/e2e/client/soak/invariants.py tests/e2e/client/soak/test_invariants.py
git commit -m "soak(py): invariants — resource / error_code / chaos / completion

Closes the hard-invariant set per §7.1.3–§7.1.6. VARIANT_ERROR_CODE_MAP
mirrors spec Appendix A exactly; chaos-causal skips sse + hits==0
variants; resource scan greps gw_*_in_flight_* series; min_completion
guards against driver infra failure."
```

---

### Task 18: `invariants.py` — soft warnings + Appendix A validation

**Files:**
- Modify: `tests/e2e/client/soak/invariants.py` (append soft warnings + validator)
- Test: append to `tests/e2e/client/soak/test_invariants.py`

Per spec §7.2 (thresholds pinned in §11.2). Soft warnings return `status: "ok" | "warn"` (not `passed`).

- [ ] **Step 1: Write the failing test**

Append to `tests/e2e/client/soak/test_invariants.py`:

```python
from soak.invariants import (
    soft_success_rate,
    soft_latency_p99,
    soft_completion_rate,
    soft_records_to_2xx_ratio,
    validate_variant_map_against_catalog,
)


def test_soft_success_rate_ok():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.by_status = {200: 100}
    leaf = soft_success_rate(rpt, chaos_mode="off")
    assert leaf["status"] == "ok"


def test_soft_success_rate_warn():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.by_status = {200: 50, 500: 50}
    leaf = soft_success_rate(rpt, chaos_mode="off")
    assert leaf["status"] == "warn"


def test_soft_latency_p99_ok():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.latency_ms_p99 = 150.0
    leaf = soft_latency_p99(rpt, chaos_mode="off")
    assert leaf["status"] == "ok"


def test_soft_latency_p99_warn():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.latency_ms_p99 = 300.0
    leaf = soft_latency_p99(rpt, chaos_mode="off")
    assert leaf["status"] == "warn"


def test_soft_completion_rate_ok():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.requests_completed = 95
    leaf = soft_completion_rate(rpt, chaos_mode="off")
    assert leaf["status"] == "ok"


def test_soft_records_to_2xx_warn_on_double_emit():
    rpt = SoakDriverReport.empty(target_count=100)
    rpt.by_status = {200: 10}
    leaf = soft_records_to_2xx_ratio(rpt, records_delta=15)
    assert leaf["status"] == "warn"
    assert leaf["raw"]["ratio"] == 1.5


def test_validate_variant_map_passes_for_full_catalog():
    full_catalog = {
        "GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_CANCELLED",
        "GW_UPSTREAM_TIMEOUT", "GW_UPSTREAM_OVERLOADED", "GW_UPSTREAM_5XX",
    }
    err = validate_variant_map_against_catalog(full_catalog)
    assert err is None


def test_validate_variant_map_fails_on_missing():
    err = validate_variant_map_against_catalog({"GW_UPSTREAM_TIMEOUT"})
    assert err is not None
    assert "GW_UPSTREAM_CONNECT_FAILED" in err
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_invariants.py -k "soft or validate_variant" -v
```

Expected: ImportError.

- [ ] **Step 3: Implement soft warnings + validator**

Append to `tests/e2e/client/soak/invariants.py`:

```python
# ── Soft warnings (§7.2; thresholds §11.2) ───────────────────────────────

_SOFT_THRESHOLDS = {
    "off":   {"success_rate_2xx_pct": 99.9, "latency_p99_ms": 200,
              "completion_rate_pct": 90,  "thread_cpu_max_pct": 80,
              "chaos_variant_hit_deviation_pct": None,
              "records_to_2xx_ratio_max": 1.10},
    "light": {"success_rate_2xx_pct": 95.0, "latency_p99_ms": 300,
              "completion_rate_pct": 80,  "thread_cpu_max_pct": 80,
              "chaos_variant_hit_deviation_pct": 25,
              "records_to_2xx_ratio_max": 1.10},
    "heavy": {"success_rate_2xx_pct": 80.0, "latency_p99_ms": 500,
              "completion_rate_pct": 60,  "thread_cpu_max_pct": 80,
              "chaos_variant_hit_deviation_pct": 25,
              "records_to_2xx_ratio_max": 1.10},
}


def _soft_leaf(path: str, source: str, raw: dict, status: str) -> dict:
    return {"path": path, "kind": "soft", "status": status,
            "source": source, "raw": raw}


def soft_success_rate(rpt: SoakDriverReport, *, chaos_mode: str) -> dict:
    completed = rpt.requests_completed
    s2xx = sum(c for s, c in rpt.by_status.items() if 200 <= s < 300)
    pct = (s2xx / completed * 100.0) if completed else 0.0
    th = _SOFT_THRESHOLDS[chaos_mode]["success_rate_2xx_pct"]
    return _soft_leaf(
        "results.soft_warnings.success_rate_2xx_pct",
        "driver.by_status",
        {"actual_pct": round(pct, 2), "threshold_pct": th,
         "completed": completed, "count_2xx": s2xx},
        "ok" if pct >= th else "warn",
    )


def soft_latency_p99(rpt: SoakDriverReport, *, chaos_mode: str) -> dict:
    p99 = rpt.latency_ms_p99
    th = _SOFT_THRESHOLDS[chaos_mode]["latency_p99_ms"]
    status = "ok" if (p99 is not None and p99 < th) else "warn"
    return _soft_leaf(
        "results.soft_warnings.latency_p99_ms",
        "driver.latency_ms_p99",
        {"actual_ms": p99, "threshold_ms": th},
        status,
    )


def soft_completion_rate(rpt: SoakDriverReport, *, chaos_mode: str) -> dict:
    pct = (rpt.requests_completed / rpt.target_count * 100.0) if rpt.target_count else 0.0
    th = _SOFT_THRESHOLDS[chaos_mode]["completion_rate_pct"]
    return _soft_leaf(
        "results.soft_warnings.completion_rate_pct",
        "driver.requests_completed / target_count",
        {"actual_pct": round(pct, 2), "threshold_pct": th},
        "ok" if pct >= th else "warn",
    )


def soft_records_to_2xx_ratio(rpt: SoakDriverReport, *, records_delta: int) -> dict:
    s2xx = sum(c for s, c in rpt.by_status.items() if 200 <= s < 300) or 1
    ratio = records_delta / s2xx
    th = 1.10
    return _soft_leaf(
        "results.soft_warnings.records_to_2xx_ratio",
        "fake_web.usage_records_delta / driver.by_status[2xx]",
        {"ratio": round(ratio, 3), "threshold_max": th,
         "records_delta": records_delta, "count_2xx": s2xx},
        "ok" if ratio <= th else "warn",
    )


# ── Startup validator: spec Appendix A vs codes.rs ───────────────────────

def validate_variant_map_against_catalog(catalog: set[str]) -> str | None:
    """Per spec §10 + C6 — every code in VARIANT_ERROR_CODE_MAP must be in
    `catalog` (parsed from codes.rs). Returns an error string if any
    mapping is stale, else None."""
    all_codes = set()
    for codes in VARIANT_ERROR_CODE_MAP.values():
        all_codes.update(codes)
    missing = sorted(all_codes - catalog)
    if missing:
        return (
            f"variant→error_code mapping references codes not in codes.rs "
            f"catalog: {missing}. Update VARIANT_ERROR_CODE_MAP in "
            f"soak/invariants.py and Appendix A of the spec."
        )
    return None
```

- [ ] **Step 4: Run + commit**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_invariants.py -v
```

Expected: 30 PASS.

```bash
git add tests/e2e/client/soak/invariants.py tests/e2e/client/soak/test_invariants.py
git commit -m "soak(py): soft warnings + Appendix A startup validator

Soft thresholds pinned per §11.2 (off/light/heavy). Validator
enforces C6: VARIANT_ERROR_CODE_MAP must be a subset of codes.rs
catalog — drift fails the orchestrator before any traffic."
```

---

### Task 19: `report.py` — JSON tree + JSONL emit + verdict

**Files:**
- Create: `tests/e2e/client/soak/report.py`
- Test: `tests/e2e/client/soak/test_report.py`

Per spec §8. Two files per run: `<ts>.json` (full tree) + `<ts>.jsonl` (one line per leaf).

- [ ] **Step 1: Write the failing test**

Create `tests/e2e/client/soak/test_report.py`:

```python
import json
from pathlib import Path

from soak.driver import SoakDriverReport
from soak.report import compose_report, write_reports


def _sample_leaf(path: str, passed: bool) -> dict:
    return {"path": path, "kind": "hard", "passed": passed,
            "source": "test", "raw": {}}


def test_compose_report_builds_tree_with_verdict():
    invariants = [
        _sample_leaf("results.invariants.process_alive.no_panic", True),
        _sample_leaf("results.invariants.account_consistency.records_equals_upstream_hits", False),
        _sample_leaf("results.invariants.resource_release.all_in_flight_gauges_zero", True),
    ]
    soft_warnings: list[dict] = []
    spectra: dict[str, dict] = {}
    rpt = SoakDriverReport.empty(target_count=49980)
    rpt.duration_ms = 60123
    tree = compose_report(
        scenario="chaos-heavy-60s",
        config={"duration_s": 60, "target_rps": 833, "chaos_mode": "heavy"},
        timeline={"t0_iso": "2026-05-22T14:30:00Z",
                  "driver_t1_iso": "2026-05-22T14:31:00Z"},
        driver_report=rpt,
        invariants=invariants,
        soft_warnings=soft_warnings,
        spectra=spectra,
    )
    assert tree["soak_run"]["config"]["chaos_mode"] == "heavy"
    v = tree["soak_run"]["results"]["verdict"]
    assert v["overall"] == "FAIL"
    assert v["hard_invariants_total"] == 3
    assert v["hard_invariants_failed"] == 1
    assert v["first_failure_path"] == \
        "results.invariants.account_consistency.records_equals_upstream_hits"


def test_write_reports_emits_json_and_jsonl(tmp_path: Path):
    tree = {"soak_run": {"id": "x", "results": {
        "invariants": {}, "verdict": {"overall": "PASS"}}}}
    out = write_reports(tmp_path, scenario="smoke", tree=tree, leaves=[
        {"path": "results.invariants.x", "kind": "hard", "passed": True,
         "source": "t", "raw": {}}
    ])
    assert out["json_path"].exists()
    assert out["jsonl_path"].exists()
    json.loads(out["json_path"].read_text())  # valid JSON
    lines = out["jsonl_path"].read_text().splitlines()
    assert len(lines) >= 2  # config + verdict + at least one leaf
    for line in lines:
        json.loads(line)  # each line is valid JSON
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_report.py -v
```

Expected: ImportError.

- [ ] **Step 3: Implement `report.py`**

Create `tests/e2e/client/soak/report.py`:

```python
"""Report writer — produces the JSON tree + JSONL files (spec §8).

`compose_report` builds the in-memory tree from the invariant evaluators'
leaves plus the driver/window/spectra inputs. `write_reports` writes both
files atomically to the timestamped directory.
"""
from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

from soak.driver import SoakDriverReport


def _utc_ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M-%SZ")


def _set_path(root: dict, dotted: str, value: dict) -> None:
    """Set `root[a][b][c] = value` for dotted `a.b.c`. Creates dicts on the way."""
    parts = dotted.split(".")
    cur: dict = root
    for p in parts[:-1]:
        cur = cur.setdefault(p, {})
    cur[parts[-1]] = value


def compose_report(
    *,
    scenario: str,
    config: dict,
    timeline: dict,
    driver_report: SoakDriverReport,
    invariants: list[dict],
    soft_warnings: list[dict],
    spectra: dict,
) -> dict:
    root: dict = {"soak_run": {
        "id": f"{timeline.get('t0_iso', _utc_ts())}-{scenario}",
        "config": config,
        "timeline": timeline,
        "results": {
            "invariants": {},
            "soft_warnings": {},
            "spectra": spectra,
        },
    }}

    # Hard invariant leaves placed at their declared paths.
    for leaf in invariants:
        _set_path(root, leaf["path"], leaf)
    for leaf in soft_warnings:
        _set_path(root, leaf["path"], leaf)

    # Verdict.
    hard_total = len(invariants)
    failed = [l for l in invariants if l["passed"] is not True]
    root["soak_run"]["results"]["verdict"] = {
        "hard_invariants_total": hard_total,
        "hard_invariants_passed": hard_total - len(failed),
        "hard_invariants_failed": len(failed),
        "first_failure_path": failed[0]["path"] if failed else None,
        "all_failures": [l["path"] for l in failed],
        "overall": "PASS" if not failed else "FAIL",
    }
    # Driver-side latency/throughput summary parked under spectra.
    root["soak_run"]["results"]["spectra"]["driver_summary"] = {
        "source": "driver",
        "raw": {
            "duration_ms": driver_report.duration_ms,
            "requests_sent": driver_report.requests_sent,
            "requests_completed": driver_report.requests_completed,
            "requests_failed_client_side": driver_report.requests_failed_client_side,
            "by_status": driver_report.by_status,
            "by_error_code": driver_report.by_error_code,
            "by_protocol": driver_report.by_protocol,
            "by_shape": driver_report.by_shape,
            "by_cross_proto": {str(k): v for k, v in driver_report.by_cross_proto.items()},
            "latency_ms": {
                "p50": driver_report.latency_ms_p50,
                "p90": driver_report.latency_ms_p90,
                "p99": driver_report.latency_ms_p99,
                "max": driver_report.latency_ms_max,
            },
            "bytes_received_total": driver_report.bytes_received_total,
        },
    }
    return root


def write_reports(
    out_dir: Path,
    *,
    scenario: str,
    tree: dict,
    leaves: list[dict],
) -> dict:
    """Write `<out_dir>/<ts>-<scenario>.json` + `.jsonl`. Returns the paths."""
    out_dir.mkdir(parents=True, exist_ok=True)
    ts = _utc_ts()
    json_path = out_dir / f"{ts}-{scenario}.json"
    jsonl_path = out_dir / f"{ts}-{scenario}.jsonl"

    json_path.write_text(json.dumps(tree, indent=2, sort_keys=False))

    with jsonl_path.open("w") as f:
        config = tree["soak_run"].get("config", {})
        f.write(json.dumps({"path": "results.config", "data": config}) + "\n")
        for leaf in leaves:
            f.write(json.dumps(leaf) + "\n")
        for k, v in tree["soak_run"]["results"].get("spectra", {}).items():
            f.write(json.dumps({"path": f"results.spectra.{k}", "data": v}) + "\n")
        f.write(json.dumps({"path": "results.verdict",
                            **tree["soak_run"]["results"]["verdict"]}) + "\n")
    return {"json_path": json_path, "jsonl_path": jsonl_path}
```

- [ ] **Step 4: Run + commit**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest soak/test_report.py -v
```

Expected: 2 PASS.

```bash
git add tests/e2e/client/soak/report.py tests/e2e/client/soak/test_report.py
git commit -m "soak(py): report.py — JSON tree + JSONL writer

compose_report builds the nested tree per spec §8.1, placing each
invariant leaf at its declared dot-path and computing the verdict
(first_failure_path / all_failures / overall). write_reports emits
both the tree JSON and a JSONL stream where every line is a
self-describing entry."
```

---

## Phase 6 — Orchestrator + wiring + verification

### Task 20: docker-compose env + Makefile target + pytest marker

**Files:**
- Modify: `tests/e2e/docker-compose.yml` (add `FAKE_UPSTREAM_CHAOS` env)
- Modify: `Makefile` (add `e2e-soak` target)
- Modify: `tests/e2e/client/pytest.ini` (register `soak` marker, exclude by default)

- [ ] **Step 1: Edit `tests/e2e/docker-compose.yml`**

Locate the `fake-upstream` service and add an `environment` block:

```yaml
  fake-upstream:
    build:
      context: ../..
      dockerfile: Dockerfile.e2e-fake-upstream
    environment:
      FAKE_UPSTREAM_CHAOS: "${FAKE_UPSTREAM_CHAOS:-off}"
    ports:
      - "9000:9000"
      - "9001:9001"
      - "9443:9443"
    healthcheck:
      test: ["CMD-SHELL", "curl -fsS http://localhost:9001/healthz || exit 1"]
      interval: 2s
      timeout: 2s
      retries: 30
```

- [ ] **Step 2: Edit `Makefile`**

Append to the existing `Makefile`:

```makefile
.PHONY: e2e-soak e2e-soak-smoke

e2e-soak:
	FAKE_UPSTREAM_CHAOS=on docker compose -f tests/e2e/docker-compose.yml up -d --build
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && .venv/bin/python3 -m pytest -m soak -v --tb=short
	docker compose -f tests/e2e/docker-compose.yml down -v

e2e-soak-smoke:
	FAKE_UPSTREAM_CHAOS=off docker compose -f tests/e2e/docker-compose.yml up -d --build
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && SOAK_SMOKE=1 .venv/bin/python3 -m pytest -m soak -k smoke -v
	docker compose -f tests/e2e/docker-compose.yml down -v
```

- [ ] **Step 3: Edit `tests/e2e/client/pytest.ini`**

Replace with:

```ini
[pytest]
asyncio_mode = auto
addopts = -ra --tb=short -m "not soak"
testpaths = scenarios soak
markers =
    soak: long-running soak / chaos tests, excluded from default `pytest` runs
```

- [ ] **Step 4: Verify default `pytest` still excludes soak**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest --collect-only 2>&1 | grep -c "test_soak" || echo "OK: no soak tests collected by default"
```

Expected: `OK: no soak tests collected by default` (test_chaos_soak.py doesn't exist yet, but if it did, the default run would skip it).

- [ ] **Step 5: Commit**

```bash
git add tests/e2e/docker-compose.yml Makefile tests/e2e/client/pytest.ini
git commit -m "soak: docker-compose env + Makefile e2e-soak + pytest marker

FAKE_UPSTREAM_CHAOS=on for the chaos runs (override via env when
calling \`make e2e-soak\`). soak marker excluded from default
\`pytest\` invocation — must opt in explicitly. CLAUDE.md will be
updated with the new target in a later task."
```

---

### Task 21: Orchestrator `test_chaos_soak.py` — smoke + chaos-off-60s + chaos-heavy-60s

**Files:**
- Create: `tests/e2e/client/scenarios/test_chaos_soak.py`
- Modify: `tests/e2e/client/conftest.py` (add `gateway_logs_since`, `gateway_container_state`, `gateway_metrics_text` helpers if not present)

Per spec §6.6. Three test functions, all `@pytest.mark.soak`.

- [ ] **Step 1: Add gateway log/state helpers to `conftest.py`**

Append to `tests/e2e/client/conftest.py` (do not duplicate; check before adding):

```python
def gateway_compose_logs_since(t0_iso: str) -> str:
    """`docker compose logs gateway --since=<t0_iso>` text."""
    proc = subprocess.run(
        ["docker", "compose", "-f", "../docker-compose.yml",
         "logs", "gateway", "--since", t0_iso, "--no-color"],
        capture_output=True, text=True, timeout=30,
    )
    return proc.stdout + proc.stderr


def gateway_container_state() -> dict:
    """`docker compose ps gateway --format json` parsed."""
    import json as _json
    proc = subprocess.run(
        ["docker", "compose", "-f", "../docker-compose.yml",
         "ps", "gateway", "--format", "json"],
        capture_output=True, text=True, timeout=15,
    )
    txt = proc.stdout.strip()
    if not txt:
        return {}
    # `compose ps --format json` emits one JSON object per service per line.
    return _json.loads(txt.splitlines()[0])


def gateway_metrics_text(metrics_url: str) -> str:
    return requests.get(f"{metrics_url.rstrip('/')}/metrics", timeout=10).text


def gateway_rss_bytes() -> int | None:
    """`docker stats --no-stream` mem usage for the gateway service.
    Returns the parsed RSS in bytes, or None if parsing fails."""
    proc = subprocess.run(
        ["docker", "stats", "--no-stream", "--format",
         "{{.MemUsage}}", "$(docker compose -f ../docker-compose.yml ps -q gateway)"],
        shell=False, capture_output=True, text=True, timeout=15,
    )
    out = proc.stdout.strip()
    if not out:
        return None
    # Format like "123.4MiB / 7.5GiB". Parse first token.
    head = out.split("/")[0].strip()
    units = {"B": 1, "KiB": 1024, "MiB": 1024**2,
             "GiB": 1024**3, "TiB": 1024**4}
    for u, mul in units.items():
        if head.endswith(u):
            try:
                return int(float(head[:-len(u)]) * mul)
            except ValueError:
                return None
    return None
```

- [ ] **Step 2: Create `scenarios/test_chaos_soak.py`**

Create the file:

```python
"""Soak / chaos orchestrator — spec §6.6 + §12.1.

Three test functions, all gated by the `@pytest.mark.soak` marker
(excluded from default pytest invocations via pytest.ini addopts).

Verification gate (spec C6): the green light from `test_soak_smoke`
is NOT sufficient evidence that the soak feature works.
`test_soak_chaos_off_60s` AND `test_soak_chaos_heavy_60s` must both
produce reports with `verdict.overall == "PASS"`.
"""
from __future__ import annotations

import os
import time
from datetime import datetime, timezone
from pathlib import Path

import pytest
import requests

from conftest import (
    FakeUpstreamClient,
    FakeWebClient,
    gateway_compose_logs_since,
    gateway_container_state,
    gateway_metrics_text,
    gateway_rss_bytes,
)
from soak.chaos_profiles import profile_for, to_scenario
from soak.codes_catalog import parse_codes_catalog
from soak.driver import SoakConfig, SoakDriver
from soak.invariants import (
    account_balance_monotonic_decreasing,
    account_drain_quiescence_reached,
    account_records_equals_upstream_hits,
    chaos_causal_per_variant,
    error_code_compliance_all_in_catalog,
    min_completion_at_least_half,
    process_alive_livez_throughout,
    process_alive_no_container_restart,
    process_alive_no_panic,
    process_alive_readyz_throughout,
    resource_all_in_flight_gauges_zero,
    resource_rss_growth_under_threshold,
    soft_completion_rate,
    soft_latency_p99,
    soft_records_to_2xx_ratio,
    soft_success_rate,
    validate_variant_map_against_catalog,
)
from soak.report import compose_report, write_reports
from soak.seed import seed_soak

REPO_ROOT = Path(__file__).resolve().parents[3]
SOAK_REPORT_DIR = Path(__file__).resolve().parents[1] / ".soak_report"


def _run_soak(
    *,
    cfg: SoakConfig,
    scenario_label: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    gateway_url: str,
    metrics_url: str,
) -> dict:
    """Drive one soak run end-to-end and return the report tree."""

    # ── 0. Pre-flight: validate Appendix A against current codes.rs ──
    catalog = parse_codes_catalog(REPO_ROOT / "crates" / "gw-core" / "src" / "error" / "codes.rs")
    err = validate_variant_map_against_catalog(catalog)
    assert err is None, err

    # ── 1. Seed entity graph + chaos scenario ─────────────────────────
    handles = seed_soak(fake_web, n_users=cfg.n_users)
    profile = profile_for(cfg.chaos_mode)
    if profile is not None:
        fake_upstream.set_scenario(to_scenario(profile))

    # ── 2. Snapshot pre-soak state ────────────────────────────────────
    snap = requests.post(
        f"{fake_web.base}/soak/window/start",
        json={"user_ids": [u[0] for u in handles.users]},
        timeout=10,
    )
    snap.raise_for_status()
    snapshot_id = snap.json()["snapshot_id"]
    t0_iso = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    rss_before = gateway_rss_bytes()
    container_before = gateway_container_state()

    # ── 3. Run the driver ─────────────────────────────────────────────
    driver_cfg = SoakConfig(
        duration_s=cfg.duration_s,
        target_rps=cfg.target_rps,
        n_threads=cfg.n_threads,
        max_inflight_per_thread=cfg.max_inflight_per_thread,
        per_request_timeout_s=cfg.per_request_timeout_s,
        drain_grace_s=cfg.drain_grace_s,
        drain_quiescence_s=cfg.drain_quiescence_s,
        drain_grace_max_s=cfg.drain_grace_max_s,
        n_users=cfg.n_users,
        chaos_mode=cfg.chaos_mode,
        gateway_url=gateway_url,
        livez_url="http://localhost:8080/livez",
        readyz_url="http://localhost:8080/readyz",
    )
    driver_report = SoakDriver(driver_cfg, handles).run()
    t1_iso = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    # ── 4. Collect window report (blocks on drain quiescence) ─────────
    window_resp = requests.get(
        f"{fake_web.base}/soak/window/report",
        params={"snapshot_id": snapshot_id},
        timeout=cfg.drain_grace_max_s + 30,
    )
    window_resp.raise_for_status()
    window = window_resp.json()

    # ── 5. Collect fake-upstream hits + metrics + container state + logs
    hits = requests.get(f"{fake_upstream.admin}/admin/hits-total", timeout=10).json()
    chaos_stats = (
        requests.get(f"{fake_upstream.admin}/admin/chaos-stats", timeout=10).json()
        if profile is not None else {"enabled": False, "variants": []}
    )
    metrics_text = gateway_metrics_text(metrics_url)
    rss_after = gateway_rss_bytes()
    container_after = gateway_container_state()
    log_text = gateway_compose_logs_since(t0_iso)

    # ── 6. Evaluate invariants ────────────────────────────────────────
    by_error_code = dict(driver_report.by_error_code)
    by_error_code.update(window["detailed_log_spectrum"].get("by_error_code", {}))
    observed_codes = set(driver_report.by_error_code) | set(
        c for c in window["detailed_log_spectrum"].get("by_error_code", {}) if c != "__absent__"
    )

    invariants = [
        process_alive_no_panic(log_text),
        process_alive_livez_throughout(driver_report),
        process_alive_readyz_throughout(driver_report),
        process_alive_no_container_restart(before=container_before, after=container_after),
        account_records_equals_upstream_hits(window=window, hits=hits),
        account_balance_monotonic_decreasing(window=window),
        account_drain_quiescence_reached(window=window),
        resource_all_in_flight_gauges_zero(metrics_text),
        resource_rss_growth_under_threshold(
            before_bytes=rss_before or 0,
            after_bytes=rss_after or 0,
            threshold_mib=100,
        ),
        error_code_compliance_all_in_catalog(
            observed=observed_codes,
            catalog=catalog,
            catalog_source=f"codes.rs@{_git_sha()}",
        ),
        min_completion_at_least_half(driver_report),
    ]
    if profile is not None:
        invariants.append(
            chaos_causal_per_variant(chaos_stats, by_error_code, min_ratio=0.7)
        )

    soft_warnings = [
        soft_success_rate(driver_report, chaos_mode=cfg.chaos_mode),
        soft_latency_p99(driver_report, chaos_mode=cfg.chaos_mode),
        soft_completion_rate(driver_report, chaos_mode=cfg.chaos_mode),
        soft_records_to_2xx_ratio(
            driver_report,
            records_delta=int(window["redis_streams"]["usage_records_delta"]),
        ),
    ]

    spectra = {
        "by_status":      {"source": "driver",   "raw": driver_report.by_status},
        "by_error_code":  {"source": "driver+fake_web", "raw": by_error_code},
        "by_protocol":    {"source": "driver",   "raw": driver_report.by_protocol},
        "by_shape":       {"source": "driver",   "raw": driver_report.by_shape},
        "by_cross_proto": {"source": "driver",
                           "raw": {str(k): v for k, v in driver_report.by_cross_proto.items()}},
        "fake_web_window": {"source": "fake_web:/soak/window/report", "raw": window},
        "fake_upstream_hits": {"source": "fake_upstream:/admin/hits-total", "raw": hits},
        "fake_upstream_chaos": {"source": "fake_upstream:/admin/chaos-stats", "raw": chaos_stats},
    }

    tree = compose_report(
        scenario=scenario_label,
        config={
            "duration_s": cfg.duration_s,
            "target_rps": cfg.target_rps,
            "n_threads": cfg.n_threads,
            "max_inflight_per_thread": cfg.max_inflight_per_thread,
            "n_users": cfg.n_users,
            "chaos_mode": cfg.chaos_mode,
            "git_sha": _git_sha(),
            "gateway_image_sha": container_after.get("Image"),
        },
        timeline={
            "t0_iso": t0_iso,
            "driver_t1_iso": t1_iso,
            "drain_quiescent_at_iso": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "drain_wall_ms": window["window_ms"],
        },
        driver_report=driver_report,
        invariants=invariants,
        soft_warnings=soft_warnings,
        spectra=spectra,
    )
    write_reports(SOAK_REPORT_DIR, scenario=scenario_label,
                  tree=tree, leaves=invariants + soft_warnings)
    return tree


def _git_sha() -> str:
    import subprocess
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "--short", "HEAD"],
            text=True, timeout=5,
        ).strip()
    except Exception:
        return "unknown"


# ── Test functions ───────────────────────────────────────────────────────

@pytest.mark.soak
def test_soak_smoke(fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient,
                    gateway_url: str, metrics_url: str) -> None:
    """Wiring validation only. NOT a feature-completion signal (spec C6)."""
    tree = _run_soak(
        cfg=SoakConfig.smoke(),
        scenario_label="smoke",
        fake_web=fake_web,
        fake_upstream=fake_upstream,
        gateway_url=gateway_url,
        metrics_url=metrics_url,
    )
    verdict = tree["soak_run"]["results"]["verdict"]
    assert verdict["overall"] == "PASS", verdict["first_failure_path"]


@pytest.mark.soak
def test_soak_chaos_off_60s(fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient,
                             gateway_url: str, metrics_url: str) -> None:
    """Full 833 rps × 60 s baseline, chaos off."""
    cfg = SoakConfig(chaos_mode="off")
    tree = _run_soak(cfg=cfg, scenario_label="chaos-off-60s",
                     fake_web=fake_web, fake_upstream=fake_upstream,
                     gateway_url=gateway_url, metrics_url=metrics_url)
    verdict = tree["soak_run"]["results"]["verdict"]
    assert verdict["overall"] == "PASS", verdict["first_failure_path"]


@pytest.mark.soak
def test_soak_chaos_heavy_60s(fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient,
                               gateway_url: str, metrics_url: str) -> None:
    """Full 833 rps × 60 s × HEAVY chaos profile. Bug-finding workload."""
    cfg = SoakConfig(chaos_mode="heavy")
    tree = _run_soak(cfg=cfg, scenario_label="chaos-heavy-60s",
                     fake_web=fake_web, fake_upstream=fake_upstream,
                     gateway_url=gateway_url, metrics_url=metrics_url)
    verdict = tree["soak_run"]["results"]["verdict"]
    assert verdict["overall"] == "PASS", verdict["first_failure_path"]
```

- [ ] **Step 3: Add `tests/e2e/client/.soak_report/` to `.gitignore`**

```bash
echo "tests/e2e/client/.soak_report/" >> .gitignore
```

- [ ] **Step 4: Confirm collection works**

```bash
cd tests/e2e/client && .venv/bin/python3 -m pytest --collect-only -m soak scenarios/test_chaos_soak.py
```

Expected: 3 tests listed (`test_soak_smoke`, `test_soak_chaos_off_60s`, `test_soak_chaos_heavy_60s`).

- [ ] **Step 5: Commit**

```bash
git add tests/e2e/client/conftest.py tests/e2e/client/scenarios/test_chaos_soak.py .gitignore
git commit -m "soak(py): orchestrator test_chaos_soak.py (3 tests)

test_soak_smoke (CI iter), test_soak_chaos_off_60s (baseline),
test_soak_chaos_heavy_60s (bug-finding). All gated by @pytest.mark.soak.
Each composes driver + fake_web + fake_upstream + metrics + logs into
the full invariant evaluation and writes JSON+JSONL reports under
.soak_report/. Spec C6 verification gate is the off/heavy tests
producing PASS reports (smoke alone is insufficient)."
```

---

### Task 22: CLAUDE.md sync — capability surface + e2e-soak target

**Files:**
- Modify: `CLAUDE.md` (Capability surface bullets + Common commands)

Per spec §12 step 7 + CLAUDE.md §Sync rule (the soak harness is a new e2e capability).

- [ ] **Step 1: Add a bullet about the soak harness under "Capability surface highlights"**

In `CLAUDE.md`, locate the bullet list under "Capability surface highlights" (after the "Tunnel" bullet) and append:

```markdown
- **Soak / chaos harness**: a 50,000-req / 60-s end-to-end soak (3 protocols × stream/non-stream × `off|light|heavy` upstream chaos) driven from `tests/e2e/client/soak/`. Hard invariants cover process liveness, strict-equality account consistency (`usage:records + dlq == fake_upstream_hits_total` at drain steady state), resource gauge release, error-code catalog compliance, and chaos causal reconciliation. Reports are AI-consumable JSON+JSONL at `tests/e2e/client/.soak_report/`. See [`docs/superpowers/specs/2026-05-22-chaos-soak-design.md`](docs/superpowers/specs/2026-05-22-chaos-soak-design.md). Gateway code is not touched — soak only consumes existing surfaces (§C1).
```

- [ ] **Step 2: Add `make e2e-soak` to "Common commands"**

Locate the "Common commands" code block and append (after the existing e2e benches block):

```bash
# Soak / chaos harness. Long-running; not in default `make e2e`.
make e2e-soak        # full 833 rps × 60s × off + heavy (spec §12.1 verification gate)
make e2e-soak-smoke  # 100 rps × 10s smoke for driver/seed iteration
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md — soak/chaos harness capability + e2e-soak commands

Per CLAUDE.md §Sync rule (capability surface bullet) and design
spec §12 step 7."
```

---

### Task 23: Verification gate — run the two 60s soaks and produce PASS reports

This task IS the verification per spec C6 + §12.1. There is no test to write — the work is to execute, observe, and act on outcomes.

- [ ] **Step 1: Bring the stack up with chaos enabled**

```bash
make e2e-soak-smoke
```

Expected: docker-compose builds, smoke test runs ~10 s + ~30 s drain wait, prints PASS. A report file appears at `tests/e2e/client/.soak_report/<ts>-smoke.json`. If smoke fails, fix the failure surfaced in the report's `verdict.first_failure_path` BEFORE proceeding to the full soaks. **Smoke green is necessary but not sufficient (C6).**

- [ ] **Step 2: Run the full chaos-off baseline**

```bash
docker compose -f tests/e2e/docker-compose.yml up -d --build
./tests/e2e/wait_for_ready.sh
cd tests/e2e/client && .venv/bin/python3 -m pytest -m soak \
    scenarios/test_chaos_soak.py::test_soak_chaos_off_60s -v --tb=long
```

Expected: ~90 s wall clock (60 s drive + 10–30 s drain). The report at `tests/e2e/client/.soak_report/<ts>-chaos-off-60s.json` must have `results.verdict.overall == "PASS"`.

If any hard invariant fails:

1. `jq '.soak_run.results.verdict.first_failure_path' < <report>.json` — get the failing leaf path.
2. `jq ".soak_run.results.invariants.<group>.<name>" < <report>.json` — inspect `raw` for evidence.
3. Diagnose per the report's `source` field. Do NOT relax the invariant.
4. If it is a real gateway bug, file an issue and stop. If it is a soak-side bug, fix the soak and re-run from step 1.

- [ ] **Step 3: Run the full chaos-heavy bug-finding pass**

```bash
FAKE_UPSTREAM_CHAOS=on docker compose -f tests/e2e/docker-compose.yml up -d --build
./tests/e2e/wait_for_ready.sh
cd tests/e2e/client && .venv/bin/python3 -m pytest -m soak \
    scenarios/test_chaos_soak.py::test_soak_chaos_heavy_60s -v --tb=long
```

Expected: report at `.soak_report/<ts>-chaos-heavy-60s.json` with `overall == "PASS"`. The chaos-causal invariant will populate `per_variant` ratios — sanity check that connection-class and 429 variants each have ratios ≥ 0.7.

Same diagnosis path as Step 2 if FAIL.

- [ ] **Step 4: Tear down**

```bash
docker compose -f tests/e2e/docker-compose.yml down -v
```

- [ ] **Step 5: Verify the reports are present and shaped correctly**

```bash
ls tests/e2e/client/.soak_report/
# Expected: at least one of each: *-smoke.{json,jsonl}, *-chaos-off-60s.{json,jsonl}, *-chaos-heavy-60s.{json,jsonl}

for f in tests/e2e/client/.soak_report/*-chaos-{off,heavy}-60s.json; do
    python3 -c "import json,sys; d=json.load(open('$f')); print('$f:', d['soak_run']['results']['verdict']['overall'])"
done
# Expected: every file prints "PASS"
```

This is the **C6 verification gate**. If either of the two 60s reports prints "FAIL" or is missing, the soak feature is NOT complete — restart at Step 2 / Step 3.

- [ ] **Step 6: Final summary commit (optional, for traceability)**

This step records the verification fact in git history. Reports themselves are gitignored.

```bash
git commit --allow-empty -m "soak: verification gate passed (off-60s + heavy-60s)

Per spec C6 + §12.1, both full 833 rps × 60s soaks produced
verdict.overall == PASS reports under tests/e2e/client/.soak_report/.
Reports are gitignored; this commit records the verification fact in
git history. The soak harness feature is complete."
```

---

## Verification gate summary

Per spec **C6**: the **only** acceptable evidence that this feature is complete is two reports at `tests/e2e/client/.soak_report/`:

| Report | `verdict.overall` |
|---|---|
| `<ts>-chaos-off-60s.json` | must be `PASS` |
| `<ts>-chaos-heavy-60s.json` | must be `PASS` |

Smoke alone is NOT sufficient — Task 23 step 5 is the gate. If either FAIL, the diagnosis path is in Task 23 step 2; the rule is **never relax an invariant to mask a bug**.

