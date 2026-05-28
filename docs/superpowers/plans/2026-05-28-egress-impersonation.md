# Egress Impersonation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship per-Vendor switchable upstream impersonation for ailinkmax-gateway. Default path (Pingora rustls) stays byte-identical; opted-in vendors route through an in-process BoringSSL/rquest loopback proxy producing real Chrome JA3/JA4 + HTTP/2 fingerprint + the exact headers official Claude Code / Codex / Gemini CLI clients send.

**Architecture:** Two-stage TLS stack in one binary. `gw-core::impersonate` (pure Rust) defines profile catalog, per-VendorKey deterministic identity derivation (SHA-256(key_id) seeded), and Aether-style lazy persistence into `vendor_keys.identity_snapshot`. New crate `gw-egress-impersonate` is the only crate linking `boring-sys`/`rquest`; it runs a UDS-preferred (TCP loopback fallback) HTTP/1.1 listener that the gateway's `pick_peer` targets when `Vendor.egress_mode == Impersonate{..}`. Internal headers `X-Egress-{Target,Profile,Proxy}` drive routing inside the loopback hop. Pingora's full lifecycle (filters, billing, inflight, error listeners, streaming) is preserved unchanged; rquest errors are surfaced via `X-Egress-Error-Class` response headers and mapped into existing retry/blacklist machinery.

**Tech Stack:** Rust 1.86 (workspace pin), `pingora 0.8` (rustls feature), `rquest` latest stable (BoringSSL via `boring-sys`), `hyper 1.x` + `hyper-util` for the loopback server, `tokio` UDS/TCP, `sqlx` for the migration, `moka` for turn-state cache, existing `dashmap` / `parking_lot`. Spec: `docs/superpowers/specs/2026-05-28-egress-impersonation-design.md`.

---

## Phase 0 — Sanity baseline

### Task 0.1: Lock the rustls-path golden baseline

**Files:**
- Read: existing E2E test fixtures
- Create: `crates/gw-proxy/tests/regression_rustls_baseline.rs`

Captures what "no behavior change" means **before** any code moves. Run once now → re-run after every phase.

- [ ] **Step 1: Survey existing E2E tests**

```bash
ls crates/gw-proxy/tests/ crates/gw-gateway/tests/
```

Note any test that hits a vendor + records upstream-request bytes. The `e2e-fake-upstream` crate has helpers.

- [ ] **Step 2: Write a baseline integration test that asserts upstream request bytes for a hardcoded request**

Create `crates/gw-proxy/tests/regression_rustls_baseline.rs`:

```rust
//! Baseline: with `egress_mode = Rustls` (default), the bytes the gateway
//! emits to upstream MUST be unchanged across this work. Re-run after every
//! phase; if it diverges, you've leaked impersonation behaviour into the
//! default path.

use std::sync::Arc;

#[tokio::test]
async fn rustls_path_upstream_bytes_unchanged() {
    // Spin up e2e-fake-upstream that records the raw request line + headers
    // (use whatever helper exists; if none, inline the recorder here).
    let recorder = e2e_fake_upstream::start_recorder().await;

    // Send one canonical OpenAI chat completion through the gateway,
    // pointing at the recorder as the vendor base_url.
    e2e_fake_upstream::send_canonical_openai_chat(&recorder.addr).await;

    let captured = recorder.last_request().expect("upstream request recorded");
    insta::assert_snapshot!("rustls_canonical_openai_chat", captured.to_wire_text());
}
```

(If `e2e-fake-upstream` lacks these helpers, the first commit of this task adds them — keep them generic; impersonation tests will reuse.)

- [ ] **Step 3: Run baseline; commit the resulting snapshot as the golden**

```bash
cargo test -p gw-proxy --test regression_rustls_baseline -- --nocapture
cargo insta accept
git add crates/gw-proxy/tests/regression_rustls_baseline.rs \
        crates/gw-proxy/tests/snapshots/
git commit -m "test(gw-proxy): pin rustls upstream-bytes golden for impersonation work"
```

Expected: snapshot file exists; test passes.

> If `insta` isn't already a dev-dep on `gw-proxy`, add it: `cargo add --dev --package gw-proxy insta -F yaml`.

---

## Phase 1 — Types + Entity Fields + Migration

> No behavior change. All additions are `#[serde(default)]` / nullable.
> After this phase, `cargo build --workspace` passes and the Phase 0 baseline still passes (re-run as the last task of the phase).

### Task 1.1: Create `gw-core::impersonate` module skeleton + `ImpersonateProfile`

**Files:**
- Create: `crates/gw-core/src/impersonate/mod.rs`
- Modify: `crates/gw-core/src/lib.rs` (add `pub mod impersonate;`)
- Test: `crates/gw-core/tests/impersonate_profile.rs`

- [ ] **Step 1: Write failing test**

`crates/gw-core/tests/impersonate_profile.rs`:

```rust
use gw_core::impersonate::ImpersonateProfile;

#[test]
fn impersonate_profile_serde_snake_case() {
    let serialized = serde_json::to_string(&ImpersonateProfile::ClaudeCodeChrome131).unwrap();
    assert_eq!(serialized, "\"claude_code_chrome131\"");

    let parsed: ImpersonateProfile = serde_json::from_str("\"openai_codex\"").unwrap();
    assert_eq!(parsed, ImpersonateProfile::OpenAiCodex);
}

#[test]
fn impersonate_profile_as_static_str() {
    assert_eq!(ImpersonateProfile::ClaudeCodeChrome131.as_static_str(), "claude_code_chrome131");
    assert_eq!(ImpersonateProfile::OpenAiCodex.as_static_str(), "openai_codex");
    assert_eq!(ImpersonateProfile::GeminiCli.as_static_str(), "gemini_cli");
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core --test impersonate_profile
```

Expected: FAIL `unresolved import gw_core::impersonate`.

- [ ] **Step 3: Implement**

`crates/gw-core/src/impersonate/mod.rs`:

```rust
//! Per-vendor impersonation profile types and per-VendorKey identity derivation.
//!
//! Pure-types layer (no IO / no async). The `gw-egress-impersonate` crate
//! maps each profile to a concrete `rquest::Emulation` + `Http2Config`; this
//! module is consumed by both `gw-egress-impersonate` (profile metadata)
//! and `gw-proxy` (write the forged headers in `inject_vendor_headers`).

use serde::{Deserialize, Serialize};

/// Identifies which official upstream CLI client to imitate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpersonateProfile {
    /// Anthropic — Claude Code (Stainless SDK on Node + Electron Chrome 131 fingerprint).
    ClaudeCodeChrome131,
    /// OpenAI — Codex CLI (codex_cli_rs on Chromium-class TLS).
    OpenAiCodex,
    /// Google — Gemini CLI (Node + undici, impersonated as Chrome 131 for higher pass rate).
    GeminiCli,
}

impl ImpersonateProfile {
    pub fn as_static_str(self) -> &'static str {
        match self {
            Self::ClaudeCodeChrome131 => "claude_code_chrome131",
            Self::OpenAiCodex => "openai_codex",
            Self::GeminiCli => "gemini_cli",
        }
    }
}

impl std::str::FromStr for ImpersonateProfile {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude_code_chrome131" => Ok(Self::ClaudeCodeChrome131),
            "openai_codex" => Ok(Self::OpenAiCodex),
            "gemini_cli" => Ok(Self::GeminiCli),
            other => Err(format!("unknown ImpersonateProfile: {other}")),
        }
    }
}
```

Add to `crates/gw-core/src/lib.rs` (alphabetical with other `pub mod`):

```rust
pub mod impersonate;
```

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core --test impersonate_profile
```

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/impersonate/mod.rs \
        crates/gw-core/src/lib.rs \
        crates/gw-core/tests/impersonate_profile.rs
git commit -m "feat(gw-core): add ImpersonateProfile enum + snake_case serde"
```

---

### Task 1.2: Add `VendorEgressMode` enum

**Files:**
- Modify: `crates/gw-core/src/impersonate/mod.rs`
- Test: `crates/gw-core/tests/impersonate_profile.rs` (append)

- [ ] **Step 1: Append failing tests**

```rust
use gw_core::impersonate::VendorEgressMode;

#[test]
fn vendor_egress_mode_default_is_rustls() {
    let default: VendorEgressMode = Default::default();
    assert!(matches!(default, VendorEgressMode::Rustls));
}

#[test]
fn vendor_egress_mode_serde_tagged() {
    let m = VendorEgressMode::Impersonate {
        profile: ImpersonateProfile::ClaudeCodeChrome131,
    };
    let j = serde_json::to_value(&m).unwrap();
    assert_eq!(j["mode"], "impersonate");
    assert_eq!(j["profile"], "claude_code_chrome131");

    let back: VendorEgressMode = serde_json::from_value(j).unwrap();
    assert!(matches!(
        back,
        VendorEgressMode::Impersonate { profile: ImpersonateProfile::ClaudeCodeChrome131 }
    ));

    let rust: VendorEgressMode = serde_json::from_str(r#"{"mode":"rustls"}"#).unwrap();
    assert!(matches!(rust, VendorEgressMode::Rustls));
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core --test impersonate_profile
```

Expected: FAIL on `VendorEgressMode` not found.

- [ ] **Step 3: Implement** — append to `crates/gw-core/src/impersonate/mod.rs`:

```rust
/// Per-Vendor egress selection. `Rustls` is the default and preserves the
/// existing Pingora-rustls upstream path bit-for-bit; `Impersonate` routes
/// through the in-process boring egress (gw-egress-impersonate).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum VendorEgressMode {
    Rustls,
    Impersonate { profile: ImpersonateProfile },
}

impl Default for VendorEgressMode {
    fn default() -> Self {
        Self::Rustls
    }
}
```

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core --test impersonate_profile
```

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/impersonate/mod.rs \
        crates/gw-core/tests/impersonate_profile.rs
git commit -m "feat(gw-core): add VendorEgressMode (Rustls default | Impersonate{profile})"
```

---

### Task 1.3: Add `IdentitySnapshot` + `IdentityVariable`

**Files:**
- Modify: `crates/gw-core/src/impersonate/mod.rs`
- Test: `crates/gw-core/tests/impersonate_profile.rs` (append)

- [ ] **Step 1: Append failing tests**

```rust
use gw_core::impersonate::{IdentitySnapshot, IdentityVariable};
use uuid::Uuid;

#[test]
fn identity_snapshot_claude_code_roundtrip() {
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::ClaudeCodeChrome131,
        variable: IdentityVariable::ClaudeCode {
            os: "Linux".into(),
            arch: "arm64".into(),
            node_version: "v22.14.0".into(),
            package_version: "0.70.0".into(),
            timeout: "600".into(),
            chrome_version: "131.0.6778.265".into(),
            electron_version: "37.3.0".into(),
            user_agent: "Mozilla/5.0 (...)".into(),
        },
    };
    let j = serde_json::to_value(&snap).unwrap();
    assert_eq!(j["profile_kind"], "claude_code_chrome131");
    assert_eq!(j["variable"]["profile"], "claude_code");
    let back: IdentitySnapshot = serde_json::from_value(j).unwrap();
    assert_eq!(back.profile_kind, snap.profile_kind);
}

#[test]
fn identity_snapshot_openai_codex_roundtrip() {
    let id = Uuid::new_v4();
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::OpenAiCodex,
        variable: IdentityVariable::OpenAiCodex {
            codex_version: "0.4.2".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            installation_id: id,
            user_agent: "codex_cli_rs/0.4.2 (linux; x86_64)".into(),
        },
    };
    let s = serde_json::to_string(&snap).unwrap();
    let back: IdentitySnapshot = serde_json::from_str(&s).unwrap();
    match back.variable {
        IdentityVariable::OpenAiCodex { installation_id, .. } => assert_eq!(installation_id, id),
        _ => panic!("variant mismatch"),
    }
}

#[test]
fn identity_snapshot_gemini_cli_roundtrip() {
    let id = Uuid::new_v4();
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::GeminiCli,
        variable: IdentityVariable::GeminiCli {
            cli_version: "0.1.21".into(),
            os_type: "linux".into(),
            arch: "x64".into(),
            installation_id: id,
        },
    };
    let s = serde_json::to_string(&snap).unwrap();
    let back: IdentitySnapshot = serde_json::from_str(&s).unwrap();
    assert_eq!(back.profile_kind, ImpersonateProfile::GeminiCli);
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core --test impersonate_profile
```

Expected: FAIL on missing types.

- [ ] **Step 3: Implement** — append to `mod.rs`. Add `uuid = { workspace = true }` to `crates/gw-core/Cargo.toml` if not present.

```rust
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentitySnapshot {
    pub profile_kind: ImpersonateProfile,
    pub variable: IdentityVariable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "profile", rename_all = "snake_case")]
pub enum IdentityVariable {
    ClaudeCode {
        os: String,
        arch: String,
        node_version: String,
        package_version: String,
        timeout: String,
        chrome_version: String,
        electron_version: String,
        user_agent: String,
    },
    OpenAiCodex {
        codex_version: String,
        os: String,
        arch: String,
        installation_id: Uuid,
        user_agent: String,
    },
    GeminiCli {
        cli_version: String,
        os_type: String,
        arch: String,
        installation_id: Uuid,
    },
}
```

If `uuid` workspace dep doesn't have the `serde` feature, add it in `[workspace.dependencies]`:

```toml
uuid = { version = "1", features = ["v4", "v5", "serde"] }
```

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core --test impersonate_profile
```

Expected: 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/impersonate/mod.rs \
        crates/gw-core/Cargo.toml Cargo.toml \
        crates/gw-core/tests/impersonate_profile.rs
git commit -m "feat(gw-core): add IdentitySnapshot + IdentityVariable per-profile enum"
```

---

### Task 1.4: Add `egress_mode` field to `Vendor`

**Files:**
- Modify: `crates/gw-core/src/entity.rs` (Vendor struct, ~line 158)
- Test: `crates/gw-core/tests/vendor_egress_mode.rs`

- [ ] **Step 1: Write failing test**

`crates/gw-core/tests/vendor_egress_mode.rs`:

```rust
use gw_core::entity::Vendor;
use gw_core::impersonate::{ImpersonateProfile, VendorEgressMode};

#[test]
fn vendor_egress_mode_defaults_to_rustls_when_absent() {
    let json = r#"{
        "id":"vendor-test",
        "display_name":"Test",
        "base_urls":["https://example.com/"],
        "auth_template":"Bearer {api_key}"
    }"#;
    let v: Vendor = serde_json::from_str(json).unwrap();
    assert!(matches!(v.egress_mode, VendorEgressMode::Rustls));
}

#[test]
fn vendor_egress_mode_round_trips_impersonate() {
    let json = r#"{
        "id":"vendor-test",
        "display_name":"Test",
        "base_urls":["https://example.com/"],
        "auth_template":"Bearer {api_key}",
        "egress_mode":{"mode":"impersonate","profile":"claude_code_chrome131"}
    }"#;
    let v: Vendor = serde_json::from_str(json).unwrap();
    match v.egress_mode {
        VendorEgressMode::Impersonate { profile } => {
            assert_eq!(profile, ImpersonateProfile::ClaudeCodeChrome131);
        }
        _ => panic!("expected Impersonate"),
    }
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core --test vendor_egress_mode
```

Expected: FAIL `no field egress_mode on type Vendor`.

- [ ] **Step 3: Implement** — modify `crates/gw-core/src/entity.rs::Vendor`:

```rust
// Add use at top of file (with other use statements):
use crate::impersonate::VendorEgressMode;

// Modify the Vendor struct (around line 158). The full struct after change:
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vendor {
    pub id: VendorId,
    pub display_name: String,
    pub base_urls: Vec<Url>,
    pub auth_template: String,
    pub timeout_overrides: Option<TimeoutSpec>,
    #[serde(default)]
    pub default_proxy: Option<Url>,
    #[serde(default)]
    pub contact: Option<String>,
    /// Egress selection — defaults to `Rustls` (preserves existing behaviour).
    /// `Impersonate` routes through gw-egress-impersonate's loopback proxy.
    #[serde(default)]
    pub egress_mode: VendorEgressMode,
}
```

- [ ] **Step 4: Run, verify pass + workspace compile**

```bash
cargo test -p gw-core --test vendor_egress_mode
cargo build --workspace
```

Expected: 2 tests pass; workspace still compiles (no consumer of Vendor breaks because field has default).

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/entity.rs \
        crates/gw-core/tests/vendor_egress_mode.rs
git commit -m "feat(gw-core): add Vendor.egress_mode (default Rustls, backward compatible)"
```

---

### Task 1.5: Add `identity_snapshot` field to `VendorKey` + loader hookup

**Files:**
- Modify: `crates/gw-core/src/entity.rs` (VendorKey struct)
- Modify: `crates/gw-store/src/loader.rs` (`VendorKeyRow` + `load_vendor_key`)
- Test: `crates/gw-core/tests/vendor_key_identity.rs`

- [ ] **Step 1: Write failing test (core only — loader test is integration, deferred to Task 1.6)**

```rust
use gw_core::entity::VendorKey;
use gw_core::ids::*;
use gw_core::impersonate::{IdentitySnapshot, IdentityVariable, ImpersonateProfile};
use secrecy::SecretString;
use uuid::Uuid;

#[test]
fn vendor_key_identity_snapshot_defaults_to_none() {
    // Build via constructor to exercise default initialization.
    // (In practice VendorKey is built by the loader; this just asserts
    // the field exists with Option<_> shape.)
    let _k = VendorKey {
        key_id: KeyId::from("vk-test"),
        vendor_id: VendorId::from("vendor-test"),
        api_key: SecretString::new("x".into()),
        accessible_models: vec![],
        model_mapping: Default::default(),
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority: 0, weight: 1, rpm_limit: 0, tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: gw_core::entity::KeyStatus::Active,
        tags: vec![],
        forced_capabilities: vec![],
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,    // NEW
    };
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core --test vendor_key_identity
```

Expected: FAIL `no field identity_snapshot`.

- [ ] **Step 3: Implement core change**

Modify `crates/gw-core/src/entity.rs::VendorKey` — add field at the end:

```rust
    /// Per-VendorKey persisted impersonation identity (Aether-style lazy
    /// persist). `None` for keys that have not yet emitted an impersonated
    /// request, or for keys whose Vendor uses `egress_mode = Rustls`.
    /// Populated by `ensure_identity` on first impersonated dispatch and
    /// re-derived if `profile_kind` does not match the current vendor profile.
    #[serde(default)]
    pub identity_snapshot: Option<crate::impersonate::IdentitySnapshot>,
```

- [ ] **Step 4: Hook the loader**

Modify `crates/gw-store/src/loader.rs::VendorKeyRow` (search for the struct; add field with `#[serde(default)]`) and the `load_vendor_key` constructor (assign `identity_snapshot: row.identity_snapshot`). Concrete diff:

In `VendorKeyRow`:
```rust
    #[serde(default)]
    pub identity_snapshot: Option<gw_core::impersonate::IdentitySnapshot>,
```

In the `Ok(VendorKey { ... })` constructor inside `load_vendor_key`:
```rust
            identity_snapshot: row.identity_snapshot,
```

- [ ] **Step 5: Run, verify pass + workspace compile**

```bash
cargo test -p gw-core --test vendor_key_identity
cargo build --workspace
```

Expected: pass + clean build.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-core/src/entity.rs \
        crates/gw-store/src/loader.rs \
        crates/gw-core/tests/vendor_key_identity.rs
git commit -m "feat(gw-core,gw-store): add VendorKey.identity_snapshot + loader plumbing"
```

---

### Task 1.6: Add `quota_project_id` to `OAuthCredentials` (Gemini support)

**Files:**
- Modify: `crates/gw-core/src/entity.rs::OAuthCredentials` (~line 417)
- Modify: `crates/gw-store/src/loader.rs::OAuthCredentialsRow`
- Test: `crates/gw-core/tests/oauth_quota.rs`

- [ ] **Step 1: Write failing test**

```rust
use gw_core::entity::OAuthCredentials;

#[test]
fn oauth_quota_project_id_defaults_to_none() {
    // OAuthCredentials is constructed by the loader; just exercise the field.
    use secrecy::SecretString;
    use time::OffsetDateTime;
    let o = OAuthCredentials {
        access_token: SecretString::new("a".into()),
        refresh_token: SecretString::new("r".into()),
        access_expires_at: OffsetDateTime::now_utc(),
        token_endpoint: "https://example.com/token".into(),
        scope: vec![],
        quota_project_id: None,
    };
    assert!(o.quota_project_id.is_none());
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core --test oauth_quota
```

Expected: FAIL `no field quota_project_id`.

- [ ] **Step 3: Implement**

Modify `crates/gw-core/src/entity.rs::OAuthCredentials`:

```rust
#[derive(Clone)]
pub struct OAuthCredentials {
    pub access_token: SecretString,
    pub refresh_token: SecretString,
    pub access_expires_at: OffsetDateTime,
    pub token_endpoint: String,
    pub scope: Vec<String>,
    /// Google Cloud "quota project" (`X-Goog-User-Project` header).
    /// `None` for non-Google credentials; populated for Gemini OAuth keys
    /// where the user explicitly bound the credential to a billing project.
    pub quota_project_id: Option<String>,
}
```

Update the `Debug` impl below it to include the new field (non-secret, just plain string).

Modify `crates/gw-store/src/loader.rs::OAuthCredentialsRow` to add:
```rust
    #[serde(default)]
    pub quota_project_id: Option<String>,
```

And in the `load_vendor_key` OAuth construction:
```rust
                Ok::<OAuthCredentials, anyhow::Error>(OAuthCredentials {
                    access_token,
                    refresh_token,
                    access_expires_at: o.access_expires_at,
                    token_endpoint: o.token_endpoint,
                    scope: o.scope,
                    quota_project_id: o.quota_project_id,
                })
```

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core --test oauth_quota
cargo build --workspace
```

Expected: pass + clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/entity.rs \
        crates/gw-store/src/loader.rs \
        crates/gw-core/tests/oauth_quota.rs
git commit -m "feat(gw-core): add OAuthCredentials.quota_project_id (Gemini X-Goog-User-Project)"
```

---

### Task 1.7: DB migration 0009 — `vendors.egress_mode` + `vendor_keys.identity_snapshot`

**Files:**
- Create: `crates/gw-store/migrations/0009_egress_impersonation.sql`

- [ ] **Step 1: Create migration**

```sql
-- 0009_egress_impersonation.sql
-- Per-Vendor egress selection (rustls default | impersonate {profile}).
-- Per-VendorKey persisted impersonation identity snapshot.
-- Both nullable; existing rows deserialize to defaults via #[serde(default)].
ALTER TABLE vendors      ADD COLUMN IF NOT EXISTS egress_mode       JSONB DEFAULT NULL;
ALTER TABLE vendor_keys  ADD COLUMN IF NOT EXISTS identity_snapshot JSONB DEFAULT NULL;
```

- [ ] **Step 2: Verify migration is picked up by the existing migration runner**

```bash
# Run the project's migration applier (whatever script lives in scripts/ or Makefile).
# If `make migrate` exists:
make migrate 2>&1 | tail -20
# Otherwise consult docs/dev-setup.md for the exact command.
```

Expected: `0009_egress_impersonation` applied; no errors.

- [ ] **Step 3: Verify schema**

```bash
psql "$DATABASE_URL" -c "\\d+ vendors"     | grep egress_mode
psql "$DATABASE_URL" -c "\\d+ vendor_keys" | grep identity_snapshot
```

Expected: both columns present, type `jsonb`, nullable.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-store/migrations/0009_egress_impersonation.sql
git commit -m "feat(gw-store): migration 0009 — vendors.egress_mode + vendor_keys.identity_snapshot"
```

---

### Task 1.8: Phase 1 regression check

- [ ] **Step 1: Re-run the Phase 0 baseline**

```bash
cargo test -p gw-proxy --test regression_rustls_baseline
cargo test --workspace
```

Expected: baseline still passes; no other tests broke.

- [ ] **Step 2: Commit a phase marker if anything moved**

If only the test cache / Cargo.lock changed:

```bash
git add Cargo.lock
git commit --allow-empty -m "chore: end of Phase 1 — types + entity + migration"
```

---

## Phase 2 — Pure Identity Derivation

> All code in this phase is pure (no IO). Lives in `gw-core::impersonate`.

### Task 2.1: `seeded_rng` helper

**Files:**
- Create: `crates/gw-core/src/impersonate/rng.rs`
- Modify: `crates/gw-core/src/impersonate/mod.rs` (`mod rng;`)
- Test: inline `#[cfg(test)] mod tests` in `rng.rs`

- [ ] **Step 1: Write failing test** — create `rng.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::seeded_rng;
    use rand::RngCore;

    #[test]
    fn same_seed_produces_identical_stream() {
        let mut a = seeded_rng("vk-abc");
        let mut b = seeded_rng("vk-abc");
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seed_produces_different_stream() {
        let mut a = seeded_rng("vk-abc");
        let mut b = seeded_rng("vk-def");
        let mut diverged = false;
        for _ in 0..16 {
            if a.next_u64() != b.next_u64() {
                diverged = true;
                break;
            }
        }
        assert!(diverged, "different seeds must diverge within 16 draws");
    }
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core impersonate::rng
```

Expected: FAIL `function seeded_rng not found`.

- [ ] **Step 3: Implement** — full `rng.rs`:

```rust
//! Deterministic RNG seeded by SHA-256(key_id) — used to pick per-VendorKey
//! identity fields from the profile pools so the same key always projects the
//! same impersonation identity (matches Aether's per-ProviderAPIKey fingerprint).

use rand::{rngs::StdRng, SeedableRng};
use sha2::{Digest, Sha256};

/// Build a deterministic 64-bit-seeded `StdRng` from any byte-like seed.
pub fn seeded_rng(seed: impl AsRef<[u8]>) -> StdRng {
    let digest = Sha256::digest(seed.as_ref());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    StdRng::from_seed(bytes)
}

#[cfg(test)]
mod tests { /* moved above */ }
```

Add `rand` / `sha2` to `crates/gw-core/Cargo.toml` if not already there (they're in workspace deps).

Add `pub mod rng;` to `crates/gw-core/src/impersonate/mod.rs`.

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core impersonate::rng
```

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/impersonate/rng.rs crates/gw-core/src/impersonate/mod.rs
git commit -m "feat(gw-core): seeded_rng helper for deterministic identity derivation"
```

---

### Task 2.2: Claude Code identity derive + headers

**Files:**
- Create: `crates/gw-core/src/impersonate/claude_code.rs`
- Modify: `crates/gw-core/src/impersonate/mod.rs` (`mod claude_code;` + re-export)
- Test: inline in `claude_code.rs`

- [ ] **Step 1: Write failing tests**

In a new `crates/gw-core/src/impersonate/claude_code.rs` skeleton, put the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::impersonate::IdentityVariable;

    #[test]
    fn derive_is_deterministic() {
        let a = derive("vk-stable-123");
        let b = derive("vk-stable-123");
        assert_eq!(a, b);
    }

    #[test]
    fn different_keys_diverge() {
        let a = derive("vk-A");
        let b = derive("vk-B");
        assert_ne!(a, b);
    }

    #[test]
    fn pool_values_only() {
        let v = derive("vk-test");
        let IdentityVariable::ClaudeCode { os, arch, package_version, .. } = v else {
            panic!("wrong variant");
        };
        assert!(["Linux", "MacOS", "Windows"].contains(&os.as_str()));
        assert!(["x64", "arm64"].contains(&arch.as_str()));
        assert!(["0.68.0", "0.69.0", "0.70.0", "0.71.0"].contains(&package_version.as_str()));
    }

    #[test]
    fn headers_static_keys_present() {
        let v = derive("vk-h");
        let h = headers(&v, /*is_stream*/ false);
        let keys: std::collections::HashSet<_> = h.iter().map(|(k, _)| k.as_str()).collect();
        for required in [
            "X-Stainless-Lang", "X-Stainless-Runtime", "X-Stainless-Retry-Count",
            "X-App", "Anthropic-Dangerous-Direct-Browser-Access",
            "anthropic-version", "Accept",
            "X-Stainless-OS", "X-Stainless-Arch",
            "X-Stainless-Runtime-Version", "X-Stainless-Package-Version",
            "X-Stainless-Timeout", "User-Agent",
            "anthropic-beta",
        ] {
            assert!(keys.contains(required), "missing header: {required}");
        }
        assert!(!keys.contains("x-stainless-helper-method"));
    }

    #[test]
    fn stream_adds_helper_method_header() {
        let v = derive("vk-stream");
        let h = headers(&v, /*is_stream*/ true);
        assert!(h.iter().any(|(k, val)| k == "x-stainless-helper-method" && val == "stream"));
    }
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core impersonate::claude_code
```

Expected: FAIL on missing fns.

- [ ] **Step 3: Implement** — the full `claude_code.rs`:

```rust
//! Claude Code profile — directly mirrors Aether's `fingerprint.py` pools and
//! `claude_code/constants.py` static header set. Re-derives deterministically
//! per VendorKey via `crate::impersonate::rng::seeded_rng`.

use crate::impersonate::rng::seeded_rng;
use crate::impersonate::IdentityVariable;
use rand::seq::SliceRandom;

// ---- Pools (mirrors Aether) ----
const OS_POOL: &[&str] = &["Linux", "MacOS", "Windows"];
const ARCH_POOL: &[&str] = &["x64", "arm64"];
const NODE_VERSION_POOL: &[&str] = &["v20.18.1", "v22.12.0", "v22.14.0", "v24.13.0"];
const PACKAGE_VERSION_POOL: &[&str] = &["0.68.0", "0.69.0", "0.70.0", "0.71.0"];
const TIMEOUT_POOL: &[&str] = &["600", "900"];
const ELECTRON_POOL: &[&str] = &["35.5.1", "36.7.1", "37.3.0", "38.7.0", "39.2.3"];

// Chrome 131 is our pinned TLS impersonate preset (matches the profile name).
// Pool entries control only the UA's Chrome/<x>.<y>.<z> tag — TLS remains Chrome131.
const CHROME_VERSION_POOL: &[&str] = &[
    "131.0.6778.265", "131.0.6778.139", "131.0.6778.108",
];

// ---- Anthropic constants (mirror Aether claude_code/constants.py) ----
const ANTHROPIC_VERSION: &str = "2023-06-01";
const BETA_TOKENS: &[&str] = &[
    "claude-code-20250219",
    "oauth-2025-04-20",
    "interleaved-thinking-2025-05-14",
];

pub fn derive(key_id: &str) -> IdentityVariable {
    let mut rng = seeded_rng(key_id);
    let os = pick(OS_POOL, &mut rng).to_string();
    let arch = pick(ARCH_POOL, &mut rng).to_string();
    let node_version = pick(NODE_VERSION_POOL, &mut rng).to_string();
    let package_version = pick(PACKAGE_VERSION_POOL, &mut rng).to_string();
    let timeout = pick(TIMEOUT_POOL, &mut rng).to_string();
    let chrome_version = pick(CHROME_VERSION_POOL, &mut rng).to_string();
    let electron_version = pick(ELECTRON_POOL, &mut rng).to_string();
    let platform_token = platform_token(&os, &arch);
    let user_agent = format!(
        "Mozilla/5.0 ({platform_token}) AppleWebKit/537.36 (KHTML, like Gecko) \
Chrome/{chrome_version} Electron/{electron_version} Safari/537.36"
    );
    IdentityVariable::ClaudeCode {
        os, arch, node_version, package_version, timeout,
        chrome_version, electron_version, user_agent,
    }
}

pub fn headers(v: &IdentityVariable, is_stream: bool) -> Vec<(String, String)> {
    let IdentityVariable::ClaudeCode {
        os, arch, node_version, package_version, timeout, user_agent, ..
    } = v else {
        panic!("claude_code::headers called with non-ClaudeCode variant");
    };
    let mut h = vec![
        ("X-Stainless-Lang".into(), "js".into()),
        ("X-Stainless-Runtime".into(), "node".into()),
        ("X-Stainless-Retry-Count".into(), "0".into()),
        ("X-App".into(), "cli".into()),
        ("Anthropic-Dangerous-Direct-Browser-Access".into(), "true".into()),
        ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
        ("Accept".into(), "application/json".into()),
        ("X-Stainless-OS".into(), os.clone()),
        ("X-Stainless-Arch".into(), arch.clone()),
        ("X-Stainless-Runtime-Version".into(), node_version.clone()),
        ("X-Stainless-Package-Version".into(), package_version.clone()),
        ("X-Stainless-Timeout".into(), timeout.clone()),
        ("User-Agent".into(), user_agent.clone()),
        ("anthropic-beta".into(), BETA_TOKENS.join(",")),
    ];
    if is_stream {
        h.push(("x-stainless-helper-method".into(), "stream".into()));
    }
    h
}

fn pick<'a>(pool: &'a [&'a str], rng: &mut impl rand::Rng) -> &'a str {
    pool.choose(rng).copied().expect("non-empty pool")
}

fn platform_token(os: &str, arch: &str) -> String {
    match (os, arch) {
        ("Linux", "arm64") => "X11; Linux arm64".into(),
        ("Linux", _) => "X11; Linux x86_64".into(),
        ("Windows", _) => "Windows NT 10.0; Win64; x64".into(),
        ("MacOS", "arm64") => "Macintosh; ARM Mac OS X 14_0_0".into(),
        ("MacOS", _) => "Macintosh; Intel Mac OS X 10_15_7".into(),
        _ => "X11; Linux x86_64".into(),
    }
}

#[cfg(test)]
mod tests { /* moved above */ }
```

Add to `crates/gw-core/src/impersonate/mod.rs`:

```rust
pub mod claude_code;
```

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core impersonate::claude_code
```

Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/impersonate/claude_code.rs crates/gw-core/src/impersonate/mod.rs
git commit -m "feat(gw-core): claude_code identity derive + headers (mirrors Aether pools)"
```

---

### Task 2.3: OpenAI Codex identity derive + headers

**Files:**
- Create: `crates/gw-core/src/impersonate/openai_codex.rs`
- Modify: `crates/gw-core/src/impersonate/mod.rs` (`mod openai_codex;`)
- Modify: `crates/gw-core/src/impersonate/openai_codex.rs` test module
- New: `crates/gw-core/src/impersonate/turn_state.rs` (Moka cache)

The Codex profile has a per-request dynamic header `x-codex-turn-state` that needs sticky state per `(key_id, conversation_id)`.

- [ ] **Step 1: Write failing test for `derive` + `headers` (static / per-key only)**

`crates/gw-core/src/impersonate/openai_codex.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::impersonate::IdentityVariable;

    #[test]
    fn derive_is_deterministic() {
        assert_eq!(derive("vk-x"), derive("vk-x"));
    }

    #[test]
    fn installation_id_is_uuid_v5_stable_per_key() {
        let IdentityVariable::OpenAiCodex { installation_id: a, .. } = derive("vk-z") else {
            panic!()
        };
        let IdentityVariable::OpenAiCodex { installation_id: b, .. } = derive("vk-z") else {
            panic!()
        };
        assert_eq!(a, b);
    }

    #[test]
    fn headers_set_includes_static_and_per_key() {
        let v = derive("vk-y");
        // Pass empty turn_state for unit test (we'll handle dynamic separately).
        let h = headers(&v, /*is_stream*/ true, &PerRequest {
            session_id: uuid::Uuid::nil(),
            thread_id: uuid::Uuid::nil(),
            window_id: uuid::Uuid::nil(),
            turn_state: "deadbeef".into(),
        });
        let keys: std::collections::HashSet<_> = h.iter().map(|(k, _)| k.as_str()).collect();
        for required in [
            "Accept", "OpenAI-Beta", "User-Agent",
            "x-codex-installation-id",
            "session-id", "thread-id", "x-client-request-id",
            "x-codex-window-id", "x-codex-turn-state",
        ] {
            assert!(keys.contains(required), "missing header: {required}");
        }
    }

    #[test]
    fn stream_accept_is_event_stream() {
        let v = derive("vk-stream");
        let h = headers(&v, true, &PerRequest::dummy());
        let accept = h.iter().find(|(k, _)| k == "Accept").unwrap().1.clone();
        assert_eq!(accept, "text/event-stream");
    }

    #[test]
    fn nonstream_accept_is_json() {
        let v = derive("vk-nonstream");
        let h = headers(&v, false, &PerRequest::dummy());
        let accept = h.iter().find(|(k, _)| k == "Accept").unwrap().1.clone();
        assert_eq!(accept, "application/json");
    }

    impl PerRequest {
        fn dummy() -> Self {
            Self {
                session_id: uuid::Uuid::nil(),
                thread_id: uuid::Uuid::nil(),
                window_id: uuid::Uuid::nil(),
                turn_state: "0".into(),
            }
        }
    }
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core impersonate::openai_codex
```

- [ ] **Step 3: Implement**

```rust
//! OpenAI Codex profile — headers per codex-rs source observation.
//! Note: `OpenAI-Beta` value pinned to `responses=v1`; verify against current
//! codex-rs source before Phase 5 rollout (spec §11 #1).

use crate::impersonate::rng::seeded_rng;
use crate::impersonate::IdentityVariable;
use rand::seq::SliceRandom;
use uuid::Uuid;

const CODEX_VERSION_POOL: &[&str] = &["0.4.0", "0.4.1", "0.4.2", "0.5.0"];
const OS_POOL: &[&str] = &["linux", "macos", "windows"];
const ARCH_POOL: &[&str] = &["x86_64", "aarch64"];
const OPENAI_BETA: &str = "responses=v1";
const CODEX_INSTALL_NAMESPACE: &str = "codex:install:";

pub struct PerRequest {
    pub session_id: Uuid,
    pub thread_id: Uuid,
    pub window_id: Uuid,
    /// Hex-encoded sticky token resolved by the gateway via
    /// `turn_state::get_or_create` keyed by `(key_id, conversation_id)`.
    pub turn_state: String,
}

pub fn derive(key_id: &str) -> IdentityVariable {
    let mut rng = seeded_rng(key_id);
    let codex_version = pick(CODEX_VERSION_POOL, &mut rng).to_string();
    let os = pick(OS_POOL, &mut rng).to_string();
    let arch = pick(ARCH_POOL, &mut rng).to_string();
    let installation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("{CODEX_INSTALL_NAMESPACE}{key_id}").as_bytes(),
    );
    let user_agent = format!("codex_cli_rs/{codex_version} ({os}; {arch})");
    IdentityVariable::OpenAiCodex {
        codex_version, os, arch, installation_id, user_agent,
    }
}

pub fn headers(v: &IdentityVariable, is_stream: bool, per_req: &PerRequest)
    -> Vec<(String, String)>
{
    let IdentityVariable::OpenAiCodex { installation_id, user_agent, .. } = v else {
        panic!("openai_codex::headers called with non-OpenAiCodex variant");
    };
    let accept = if is_stream { "text/event-stream" } else { "application/json" };
    vec![
        ("Accept".into(), accept.into()),
        ("OpenAI-Beta".into(), OPENAI_BETA.into()),
        ("User-Agent".into(), user_agent.clone()),
        ("x-codex-installation-id".into(), installation_id.to_string()),
        ("session-id".into(), per_req.session_id.to_string()),
        ("thread-id".into(), per_req.thread_id.to_string()),
        ("x-client-request-id".into(), per_req.thread_id.to_string()),
        ("x-codex-window-id".into(), per_req.window_id.to_string()),
        ("x-codex-turn-state".into(), per_req.turn_state.clone()),
    ]
}

fn pick<'a>(pool: &'a [&'a str], rng: &mut impl rand::Rng) -> &'a str {
    pool.choose(rng).copied().expect("non-empty pool")
}

#[cfg(test)]
mod tests { /* moved above */ }
```

Register in mod.rs: `pub mod openai_codex;`.

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core impersonate::openai_codex
```

Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/impersonate/openai_codex.rs crates/gw-core/src/impersonate/mod.rs
git commit -m "feat(gw-core): openai_codex identity derive + headers"
```

---

### Task 2.4: Gemini CLI identity derive + headers

**Files:**
- Create: `crates/gw-core/src/impersonate/gemini_cli.rs`
- Modify: `crates/gw-core/src/impersonate/mod.rs` (`mod gemini_cli;`)

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::impersonate::IdentityVariable;

    #[test]
    fn derive_is_deterministic() { assert_eq!(derive("vk-g"), derive("vk-g")); }

    #[test]
    fn user_agent_substitutes_model_per_request() {
        let v = derive("vk-1");
        let h = headers(&v, /*model*/ "gemini-1.5-pro", /*quota*/ None);
        let ua = h.iter().find(|(k, _)| k == "User-Agent").unwrap().1.clone();
        assert!(ua.contains("gemini-1.5-pro"));
    }

    #[test]
    fn quota_project_id_when_some() {
        let v = derive("vk-2");
        let h = headers(&v, "gemini-1.5-pro", Some("my-project"));
        assert!(h.iter().any(|(k, val)| k == "X-Goog-User-Project" && val == "my-project"));
    }

    #[test]
    fn quota_project_id_absent_when_none() {
        let v = derive("vk-3");
        let h = headers(&v, "gemini-1.5-pro", None);
        assert!(!h.iter().any(|(k, _)| k == "X-Goog-User-Project"));
    }

    #[test]
    fn installation_id_is_uuid_v5_stable() {
        let IdentityVariable::GeminiCli { installation_id: a, .. } = derive("vk-x") else { panic!() };
        let IdentityVariable::GeminiCli { installation_id: b, .. } = derive("vk-x") else { panic!() };
        assert_eq!(a, b);
    }
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core impersonate::gemini_cli
```

- [ ] **Step 3: Implement**

```rust
//! Gemini CLI profile — UA template & x-goog-* headers per gemini-cli TS source.
//! Note: real client uses Node + undici; we impersonate Chrome131 at TLS layer
//! for higher pass rate against Google backends (spec §11 #3).

use crate::impersonate::rng::seeded_rng;
use crate::impersonate::IdentityVariable;
use rand::seq::SliceRandom;
use uuid::Uuid;

const CLI_VERSION_POOL: &[&str] = &["0.1.18", "0.1.19", "0.1.20", "0.1.21"];
const OS_TYPE_POOL: &[&str] = &["linux", "darwin", "win32"];
const ARCH_POOL: &[&str] = &["x64", "arm64"];
const SURFACE: &str = "unknown";
const GEMINI_INSTALL_NAMESPACE: &str = "gemini:install:";

pub fn derive(key_id: &str) -> IdentityVariable {
    let mut rng = seeded_rng(key_id);
    let cli_version = pick(CLI_VERSION_POOL, &mut rng).to_string();
    let os_type = pick(OS_TYPE_POOL, &mut rng).to_string();
    let arch = pick(ARCH_POOL, &mut rng).to_string();
    let installation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("{GEMINI_INSTALL_NAMESPACE}{key_id}").as_bytes(),
    );
    IdentityVariable::GeminiCli { cli_version, os_type, arch, installation_id }
}

pub fn headers(
    v: &IdentityVariable, model: &str, quota_project_id: Option<&str>,
) -> Vec<(String, String)> {
    let IdentityVariable::GeminiCli { cli_version, os_type, arch, installation_id } = v else {
        panic!("gemini_cli::headers called with non-GeminiCli variant");
    };
    let ua = format!(
        "GeminiCLI-/{}/{} ({}; {}; {})",   // clientName empty for standard terminal
        cli_version, model, os_type, arch, SURFACE
    );
    let mut h = vec![
        ("Content-Type".into(), "application/json".into()),
        ("User-Agent".into(), ua),
        ("x-gemini-api-privileged-user-id".into(), installation_id.to_string()),
    ];
    if let Some(p) = quota_project_id {
        h.push(("X-Goog-User-Project".into(), p.into()));
    }
    h
}

fn pick<'a>(pool: &'a [&'a str], rng: &mut impl rand::Rng) -> &'a str {
    pool.choose(rng).copied().expect("non-empty pool")
}

#[cfg(test)]
mod tests { /* moved above */ }
```

Register: `pub mod gemini_cli;`.

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core impersonate::gemini_cli
```

Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/impersonate/gemini_cli.rs crates/gw-core/src/impersonate/mod.rs
git commit -m "feat(gw-core): gemini_cli identity derive + headers"
```

---

### Task 2.5: `ensure_identity` unified entry point

**Files:**
- Create: `crates/gw-core/src/impersonate/ensure.rs`
- Modify: `crates/gw-core/src/impersonate/mod.rs` (re-export `ensure_identity`)

- [ ] **Step 1: Write failing test**

`crates/gw-core/tests/ensure_identity.rs`:

```rust
use gw_core::impersonate::{ensure_identity, ImpersonateProfile, IdentitySnapshot, IdentityVariable};

#[test]
fn ensure_returns_snapshot_when_present_and_matching_profile() {
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::ClaudeCodeChrome131,
        variable: gw_core::impersonate::claude_code::derive("vk-A"),
    };
    let (resolved, persist) = ensure_identity(
        "vk-A", Some(&snap), ImpersonateProfile::ClaudeCodeChrome131
    );
    assert!(matches!(resolved.variable, IdentityVariable::ClaudeCode { .. }));
    assert!(!persist, "matching snapshot should not request persist");
}

#[test]
fn ensure_re_derives_when_profile_mismatches() {
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::OpenAiCodex,
        variable: gw_core::impersonate::openai_codex::derive("vk-A"),
    };
    let (resolved, persist) = ensure_identity(
        "vk-A", Some(&snap), ImpersonateProfile::ClaudeCodeChrome131
    );
    assert!(matches!(resolved.variable, IdentityVariable::ClaudeCode { .. }));
    assert!(persist, "mismatched profile must trigger persist");
}

#[test]
fn ensure_derives_when_snapshot_absent_and_requests_persist() {
    let (resolved, persist) = ensure_identity("vk-A", None, ImpersonateProfile::GeminiCli);
    assert!(matches!(resolved.variable, IdentityVariable::GeminiCli { .. }));
    assert!(persist);
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core --test ensure_identity
```

- [ ] **Step 3: Implement**

`crates/gw-core/src/impersonate/ensure.rs`:

```rust
use crate::impersonate::{
    claude_code, gemini_cli, openai_codex,
    IdentitySnapshot, IdentityVariable, ImpersonateProfile,
};

/// Resolved identity for the current request.
pub struct ResolvedIdentity {
    pub profile: ImpersonateProfile,
    pub variable: IdentityVariable,
}

/// Returns the resolved identity AND a boolean indicating whether the caller
/// SHOULD schedule a lazy persist of `IdentitySnapshot { profile, variable }`
/// to `vendor_keys.identity_snapshot`.
pub fn ensure_identity(
    key_id: &str,
    existing: Option<&IdentitySnapshot>,
    profile: ImpersonateProfile,
) -> (ResolvedIdentity, bool) {
    if let Some(snap) = existing {
        if snap.profile_kind == profile {
            return (
                ResolvedIdentity { profile, variable: snap.variable.clone() },
                false,
            );
        }
        // profile drift → re-derive
    }
    let variable = match profile {
        ImpersonateProfile::ClaudeCodeChrome131 => claude_code::derive(key_id),
        ImpersonateProfile::OpenAiCodex => openai_codex::derive(key_id),
        ImpersonateProfile::GeminiCli => gemini_cli::derive(key_id),
    };
    (ResolvedIdentity { profile, variable }, true)
}
```

Register & re-export in `mod.rs`:

```rust
pub mod ensure;
pub use ensure::{ensure_identity, ResolvedIdentity};
```

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core --test ensure_identity
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/impersonate/ensure.rs \
        crates/gw-core/src/impersonate/mod.rs \
        crates/gw-core/tests/ensure_identity.rs
git commit -m "feat(gw-core): ensure_identity entry + persist-needed flag"
```

---

### Task 2.6: Turn-state Moka cache for Codex

**Files:**
- Create: `crates/gw-core/src/impersonate/turn_state.rs`
- Modify: `crates/gw-core/src/impersonate/mod.rs` (`pub mod turn_state;`)

- [ ] **Step 1: Write failing test**

`crates/gw-core/tests/turn_state.rs`:

```rust
use gw_core::impersonate::turn_state::TurnStateCache;

#[tokio::test]
async fn same_conversation_returns_same_token() {
    let cache = TurnStateCache::new(std::time::Duration::from_secs(60), 1024);
    let a = cache.get_or_create("vk-1", Some("conv-A")).await;
    let b = cache.get_or_create("vk-1", Some("conv-A")).await;
    assert_eq!(a, b);
}

#[tokio::test]
async fn different_conversations_get_distinct_tokens() {
    let cache = TurnStateCache::new(std::time::Duration::from_secs(60), 1024);
    let a = cache.get_or_create("vk-1", Some("conv-A")).await;
    let b = cache.get_or_create("vk-1", Some("conv-B")).await;
    assert_ne!(a, b);
}

#[tokio::test]
async fn no_conversation_id_generates_fresh_each_call() {
    let cache = TurnStateCache::new(std::time::Duration::from_secs(60), 1024);
    let a = cache.get_or_create("vk-1", None).await;
    let b = cache.get_or_create("vk-1", None).await;
    assert_ne!(a, b);  // no key to cache by → fresh token
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-core --test turn_state
```

- [ ] **Step 3: Implement**

```rust
//! Sticky token cache for Codex `x-codex-turn-state`. Keyed by
//! `(key_id, conversation_id)`, TTL configurable (spec default 5 min).

use moka::future::Cache;
use rand::RngCore;
use std::time::Duration;

pub struct TurnStateCache {
    inner: Cache<(String, String), String>,
}

impl TurnStateCache {
    pub fn new(ttl: Duration, capacity: u64) -> Self {
        let inner = Cache::builder()
            .max_capacity(capacity)
            .time_to_live(ttl)
            .build();
        Self { inner }
    }

    /// `None` conversation_id → no cache binding (fresh token per call —
    /// matches "non-conversational request" semantics).
    pub async fn get_or_create(&self, key_id: &str, conversation_id: Option<&str>) -> String {
        match conversation_id {
            Some(cid) => {
                let k = (key_id.to_owned(), cid.to_owned());
                self.inner.get_with(k, async { random_hex(32) }).await
            }
            None => random_hex(32),
        }
    }
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}
```

Add `moka = { workspace = true, features = ["future"] }` + `hex = "0.4"` to `crates/gw-core/Cargo.toml` if not present.

Register in mod.rs: `pub mod turn_state;`.

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-core --test turn_state
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/impersonate/turn_state.rs \
        crates/gw-core/src/impersonate/mod.rs \
        crates/gw-core/Cargo.toml \
        crates/gw-core/tests/turn_state.rs
git commit -m "feat(gw-core): TurnStateCache (Moka, TTL-keyed by (key_id,conversation_id))"
```

---

### Task 2.7: Lazy persist queue (in gw-store)

**Files:**
- Create: `crates/gw-store/src/identity_persist.rs`
- Modify: `crates/gw-store/src/lib.rs`

- [ ] **Step 1: Write failing test**

`crates/gw-store/tests/identity_persist.rs`:

```rust
use gw_store::identity_persist::IdentityPersistQueue;
use std::sync::Arc;

#[tokio::test]
async fn dedup_within_pending_window() {
    // Construct queue with a noop writer for the unit test; verify dedup.
    let writer = Arc::new(noop_writer::Counter::default());
    let q = IdentityPersistQueue::new_with_writer(writer.clone(), 1024);

    let snap = sample_snapshot();
    q.schedule("vk-A".into(), snap.clone());
    q.schedule("vk-A".into(), snap.clone());
    q.schedule("vk-B".into(), snap.clone());
    q.flush_for_test().await;

    assert_eq!(writer.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn drops_when_pending_at_cap() {
    let writer = Arc::new(noop_writer::Counter::default());
    let q = IdentityPersistQueue::new_with_writer(writer.clone(), /*max_pending*/ 1);
    q.schedule("vk-A".into(), sample_snapshot());
    let dropped = q.schedule("vk-B".into(), sample_snapshot());
    assert!(dropped, "second schedule should be dropped (pending at cap)");
}

// helpers --
mod noop_writer {
    use async_trait::async_trait;
    use std::sync::atomic::AtomicUsize;
    #[derive(Default)]
    pub struct Counter { pub calls: AtomicUsize }
    #[async_trait]
    impl gw_store::identity_persist::SnapshotWriter for Counter {
        async fn write_if_null(
            &self, _key_id: &str,
            _snap: &gw_core::impersonate::IdentitySnapshot,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }
}

fn sample_snapshot() -> gw_core::impersonate::IdentitySnapshot {
    gw_core::impersonate::IdentitySnapshot {
        profile_kind: gw_core::impersonate::ImpersonateProfile::ClaudeCodeChrome131,
        variable: gw_core::impersonate::claude_code::derive("vk-sample"),
    }
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-store --test identity_persist
```

- [ ] **Step 3: Implement** — `crates/gw-store/src/identity_persist.rs`:

```rust
//! Lazy persist queue for VendorKey identity snapshots.
//!
//! Mirrors Aether's `_PENDING_LAZY_PERSIST` + `_PENDING_LAZY_PERSIST_LOCK`
//! pattern. Per-key dedup, total cap, fire-and-forget background writer.

use async_trait::async_trait;
use dashmap::DashSet;
use gw_core::impersonate::IdentitySnapshot;
use std::sync::Arc;

#[async_trait]
pub trait SnapshotWriter: Send + Sync + 'static {
    /// Write iff the DB column is NULL (no-op otherwise — preserves any
    /// admin-edited identity values).
    async fn write_if_null(&self, key_id: &str, snap: &IdentitySnapshot)
        -> anyhow::Result<()>;
}

pub struct IdentityPersistQueue {
    pending: DashSet<String>,
    max_pending: usize,
    writer: Arc<dyn SnapshotWriter>,
}

impl IdentityPersistQueue {
    pub fn new(writer: Arc<dyn SnapshotWriter>, max_pending: usize) -> Self {
        Self { pending: DashSet::new(), max_pending, writer }
    }

    #[cfg(test)]
    pub fn new_with_writer(writer: Arc<dyn SnapshotWriter>, max_pending: usize) -> Self {
        Self::new(writer, max_pending)
    }

    /// Returns `true` if the schedule was dropped (cap exceeded or duplicate
    /// — caller doesn't need to react, this is just for metrics).
    pub fn schedule(&self, key_id: String, snap: IdentitySnapshot) -> bool {
        if self.pending.len() >= self.max_pending {
            metrics::counter!("gw_egress_lazy_persist_total", "result" => "dropped").increment(1);
            return true;
        }
        if !self.pending.insert(key_id.clone()) {
            metrics::counter!("gw_egress_lazy_persist_total", "result" => "deferred").increment(1);
            return true;
        }
        let writer = Arc::clone(&self.writer);
        let pending = self.pending.clone();
        tokio::spawn(async move {
            let result = writer.write_if_null(&key_id, &snap).await;
            match result {
                Ok(_) => metrics::counter!("gw_egress_lazy_persist_total", "result" => "ok").increment(1),
                Err(e) => {
                    tracing::warn!("identity_persist failed key={key_id}: {e}");
                    metrics::counter!("gw_egress_lazy_persist_total", "result" => "failed").increment(1);
                }
            }
            pending.remove(&key_id);
        });
        false
    }

    #[cfg(test)]
    pub async fn flush_for_test(&self) {
        // Best-effort wait; under tokio this is enough for noop writer.
        tokio::task::yield_now().await;
        for _ in 0..16 {
            if self.pending.is_empty() { break; }
            tokio::task::yield_now().await;
        }
    }
}

impl Clone for DashSet<String> {
    // DashSet doesn't impl Clone; we work around via Arc in the prod ctor.
    // For test: the noop writer keeps state in writer, not pending.
    fn clone(&self) -> Self { unreachable!("use Arc<DashSet>") }
}
```

Wait — `DashSet` does not impl Clone. Rework: store `Arc<DashSet<String>>` internally so the spawned task can hold its own Arc.

Apply this correction:

```rust
pub struct IdentityPersistQueue {
    pending: Arc<DashSet<String>>,
    max_pending: usize,
    writer: Arc<dyn SnapshotWriter>,
}

impl IdentityPersistQueue {
    pub fn new(writer: Arc<dyn SnapshotWriter>, max_pending: usize) -> Self {
        Self { pending: Arc::new(DashSet::new()), max_pending, writer }
    }
    // schedule(): same, but `let pending = Arc::clone(&self.pending);` inside.
}
```

(Drop the bogus `impl Clone for DashSet<String>` above.)

Register in `crates/gw-store/src/lib.rs`:
```rust
pub mod identity_persist;
```

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-store --test identity_persist
```

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-store/src/identity_persist.rs \
        crates/gw-store/src/lib.rs \
        crates/gw-store/tests/identity_persist.rs
git commit -m "feat(gw-store): IdentityPersistQueue (Aether-style lazy persist + dedup)"
```

---

### Task 2.8: Concrete SQL `SnapshotWriter` impl

**Files:**
- Modify: `crates/gw-store/src/identity_persist.rs` (add `PgSnapshotWriter`)

- [ ] **Step 1: Add SQL writer + test (integration; requires Postgres — skipped in unit suite, run via `make integration-test`)**

Append to `identity_persist.rs`:

```rust
use sqlx::PgPool;

pub struct PgSnapshotWriter {
    pool: PgPool,
}

impl PgSnapshotWriter {
    pub fn new(pool: PgPool) -> Self { Self { pool } }
}

#[async_trait]
impl SnapshotWriter for PgSnapshotWriter {
    async fn write_if_null(
        &self, key_id: &str, snap: &IdentitySnapshot,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_value(snap)?;
        // Conditional UPDATE: only write when NULL (preserves admin edits).
        sqlx::query(
            "UPDATE vendor_keys SET identity_snapshot = $1, updated_at = NOW() \
             WHERE key_id = $2 AND identity_snapshot IS NULL"
        )
        .bind(payload)
        .bind(key_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
```

- [ ] **Step 2: Build check**

```bash
cargo build -p gw-store
```

- [ ] **Step 3: Commit**

```bash
git add crates/gw-store/src/identity_persist.rs
git commit -m "feat(gw-store): PgSnapshotWriter — conditional UPDATE on NULL"
```

---

### Task 2.9: Phase 2 regression check

- [ ] **Step 1: Re-run baseline + full test suite**

```bash
cargo test -p gw-proxy --test regression_rustls_baseline
cargo test --workspace --no-fail-fast
```

Expected: all green.

- [ ] **Step 2: Phase marker commit if Cargo.lock moved**

```bash
git add -u && git commit -m "chore: end of Phase 2 — identity derivation pure layer" || true
```

---

## Phase 3 — `gw-egress-impersonate` Crate

> Boring/rquest backend, the only crate linking `boring-sys`. Isolated from the rest of the workspace.

### Task 3.1: Crate skeleton + workspace registration

**Files:**
- Create: `crates/gw-egress-impersonate/Cargo.toml`
- Create: `crates/gw-egress-impersonate/src/lib.rs`
- Modify: root `Cargo.toml` (`members`, `[workspace.dependencies]` add `rquest`)

- [ ] **Step 1: Add workspace deps**

In root `Cargo.toml::[workspace.dependencies]` add (use the current latest stable from `crates.io` at implementation time; pin via Cargo.lock):

```toml
rquest = { version = "1", default-features = false, features = ["json", "stream", "rustls-tls"] }
# NOTE: replace `version = "1"` with the actual current major. The `rustls-tls`
# feature here is rquest's *fallback* TLS for non-emulated requests; emulation
# requests use boring internally regardless. Confirm features against current
# rquest docs before pinning.
hyper = { version = "1.4", features = ["server", "http1"] }
hyper-util = { version = "0.1", features = ["server", "server-auto", "tokio"] }
http-body-util = "0.1"
```

In root `Cargo.toml::[workspace].members`:

```toml
    "crates/gw-egress-impersonate",
```

- [ ] **Step 2: Create crate**

`crates/gw-egress-impersonate/Cargo.toml`:

```toml
[package]
name = "gw-egress-impersonate"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "In-process BoringSSL/rquest impersonating egress for gw-proxy."

[dependencies]
gw-core = { path = "../gw-core" }
tokio = { workspace = true }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
bytes = { workspace = true }
url = { workspace = true }
tracing = "0.1"
thiserror = { workspace = true }
anyhow = { workspace = true }
dashmap = { workspace = true }
parking_lot = { workspace = true }
moka = { workspace = true, features = ["sync"] }
metrics = "0.23"

rquest = { workspace = true }
hyper = { workspace = true }
hyper-util = { workspace = true }
http-body-util = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

`crates/gw-egress-impersonate/src/lib.rs`:

```rust
//! In-process BoringSSL/rquest impersonating egress.
//!
//! Spec: `docs/superpowers/specs/2026-05-28-egress-impersonation-design.md`.
//! Pingora rustls (default path) and this crate's boring backend coexist in
//! one binary; this crate runs a loopback HTTP/1 listener that the gateway
//! targets when `Vendor.egress_mode == Impersonate{..}`.

pub mod error;
pub mod handle;
pub mod pool;
pub mod profiles;
pub mod server;
pub mod supervisor;

pub use handle::{EgressBind, EgressHandle};
```

Stub each module with just `pub fn _stub() {}` to make it compile.

- [ ] **Step 3: Verify it builds**

```bash
cargo check -p gw-egress-impersonate
```

Expected: clean (modulo unused warnings).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/gw-egress-impersonate/
git commit -m "feat(gw-egress-impersonate): crate skeleton + workspace deps (rquest, hyper)"
```

---

### Task 3.2: Profile → rquest Emulation mapping

**Files:**
- Modify: `crates/gw-egress-impersonate/src/profiles.rs`
- Test: inline in `profiles.rs`

> rquest's emulation API surface: `rquest::ClientBuilder::emulation(...)` accepting an emulation provider. The exact constructor name (`Emulation::Chrome131` vs `rquest_util::Emulation::Chrome131`) depends on the rquest version. Verify against `cargo doc -p rquest --open` and substitute the right path in the code below.

- [ ] **Step 1: Write failing test**

```rust
// In profiles.rs
#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::impersonate::ImpersonateProfile;

    #[test]
    fn all_profiles_have_emulation() {
        for p in [
            ImpersonateProfile::ClaudeCodeChrome131,
            ImpersonateProfile::OpenAiCodex,
            ImpersonateProfile::GeminiCli,
        ] {
            // Just exercise the mapping; must not panic.
            let _ = emulation_for(p);
        }
    }
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-egress-impersonate profiles
```

- [ ] **Step 3: Implement**

```rust
//! Map `ImpersonateProfile` → rquest emulation config.

use gw_core::impersonate::ImpersonateProfile;
// Substitute the exact path the current rquest version exposes:
use rquest::EmulationProvider;
use rquest_util::Emulation;

pub fn emulation_for(profile: ImpersonateProfile) -> EmulationProvider {
    match profile {
        // All three currently pin Chrome131 per spec §6.3/6.4/6.5.
        ImpersonateProfile::ClaudeCodeChrome131 => Emulation::Chrome131.into(),
        ImpersonateProfile::OpenAiCodex         => Emulation::Chrome131.into(),
        ImpersonateProfile::GeminiCli           => Emulation::Chrome131.into(),
    }
}

#[cfg(test)]
mod tests { /* moved above */ }
```

Add `rquest-util = { workspace = true }` to crate Cargo.toml + to `[workspace.dependencies]`.

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-egress-impersonate profiles
```

- [ ] **Step 5: Commit**

```bash
git add crates/gw-egress-impersonate/src/profiles.rs \
        crates/gw-egress-impersonate/Cargo.toml Cargo.toml
git commit -m "feat(gw-egress-impersonate): profile → rquest Emulation mapping"
```

---

### Task 3.3: `ClientPool` with LRU

**Files:**
- Modify: `crates/gw-egress-impersonate/src/pool.rs`
- Test: `crates/gw-egress-impersonate/tests/pool.rs`

- [ ] **Step 1: Write failing test**

```rust
use gw_egress_impersonate::pool::{ClientPool, PoolKey};
use gw_core::impersonate::ImpersonateProfile;

#[tokio::test]
async fn pool_returns_cached_client_for_same_key() {
    let pool = ClientPool::new(4);
    let a = pool.get_or_create(PoolKey {
        profile: ImpersonateProfile::ClaudeCodeChrome131,
        proxy: None,
    }).await.unwrap();
    let b = pool.get_or_create(PoolKey {
        profile: ImpersonateProfile::ClaudeCodeChrome131,
        proxy: None,
    }).await.unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}

#[tokio::test]
async fn pool_evicts_lru_at_capacity() {
    let pool = ClientPool::new(2);
    let _a = pool.get_or_create(PoolKey { profile: ImpersonateProfile::ClaudeCodeChrome131, proxy: None }).await.unwrap();
    let _b = pool.get_or_create(PoolKey { profile: ImpersonateProfile::OpenAiCodex,         proxy: None }).await.unwrap();
    let _c = pool.get_or_create(PoolKey { profile: ImpersonateProfile::GeminiCli,           proxy: None }).await.unwrap();
    assert_eq!(pool.size(), 2);
}

#[tokio::test]
async fn proxy_url_normalization_doesnt_split_pool() {
    let pool = ClientPool::new(4);
    let a = pool.get_or_create(PoolKey {
        profile: ImpersonateProfile::ClaudeCodeChrome131,
        proxy: Some("http://Example.com:8080/".parse().unwrap()),
    }).await.unwrap();
    let b = pool.get_or_create(PoolKey {
        profile: ImpersonateProfile::ClaudeCodeChrome131,
        proxy: Some("http://example.com:8080".parse().unwrap()),
    }).await.unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-egress-impersonate --test pool
```

- [ ] **Step 3: Implement**

`crates/gw-egress-impersonate/src/pool.rs`:

```rust
//! rquest Client pool keyed by (profile, normalized proxy URL).
//! LRU eviction at `max_sessions`. Background eviction does an async
//! `Client::shutdown` to release upstream connections cleanly.

use crate::profiles::emulation_for;
use gw_core::impersonate::ImpersonateProfile;
use parking_lot::Mutex;
use rquest::Client;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;
use url::Url;

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct PoolKey {
    pub profile: ImpersonateProfile,
    pub proxy: Option<Url>,
}

impl PoolKey {
    fn normalize(mut self) -> Self {
        if let Some(ref mut u) = self.proxy {
            // Normalise: lowercase host, strip trailing slash, normalise port.
            if let Some(host) = u.host_str().map(|s| s.to_ascii_lowercase()) {
                let _ = u.set_host(Some(&host));
            }
            if u.path() == "/" { let _ = u.set_path(""); }
        }
        self
    }
}

struct Entry {
    client: Arc<Client>,
    last_used: Instant,
}

pub struct ClientPool {
    inner: Mutex<HashMap<PoolKey, Entry>>,
    capacity: NonZeroUsize,
}

impl ClientPool {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::with_capacity(capacity.max(1))),
            capacity: NonZeroUsize::new(capacity.max(1)).unwrap(),
        }
    }

    pub fn size(&self) -> usize { self.inner.lock().len() }

    pub async fn get_or_create(&self, key: PoolKey) -> anyhow::Result<Arc<Client>> {
        let key = key.normalize();
        {
            let mut guard = self.inner.lock();
            if let Some(entry) = guard.get_mut(&key) {
                entry.last_used = Instant::now();
                metrics::counter!("gw_egress_client_pool_total", "event" => "hit").increment(1);
                return Ok(Arc::clone(&entry.client));
            }
        }
        // Miss — build outside the lock (Client::build is potentially slow).
        let client = build_client(&key).await?;
        let arc = Arc::new(client);

        // Insert + evict if over capacity.
        let evicted = {
            let mut guard = self.inner.lock();
            guard.insert(key.clone(), Entry { client: Arc::clone(&arc), last_used: Instant::now() });
            if guard.len() > self.capacity.get() {
                // Evict the LRU.
                let lru = guard.iter()
                    .min_by_key(|(_, e)| e.last_used)
                    .map(|(k, _)| k.clone());
                lru.and_then(|k| guard.remove(&k).map(|e| (k, e)))
            } else { None }
        };
        if let Some((_, e)) = evicted {
            // Best-effort shutdown.
            let evicted_arc = e.client;
            tokio::spawn(async move { let _ = evicted_arc.shutdown().await; });
            metrics::counter!("gw_egress_client_pool_total", "event" => "evicted").increment(1);
        }
        metrics::counter!("gw_egress_client_pool_total", "event" => "miss").increment(1);
        Ok(arc)
    }
}

async fn build_client(key: &PoolKey) -> anyhow::Result<Client> {
    let mut builder = rquest::Client::builder().emulation(emulation_for(key.profile));
    if let Some(ref proxy) = key.proxy {
        builder = builder.proxy(rquest::Proxy::all(proxy.as_str())?);
    }
    Ok(builder.build()?)
}
```

> The exact rquest API to construct a Client / set emulation / set proxy may differ slightly by version. Adjust method names against `cargo doc -p rquest --open` if compilation fails.

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-egress-impersonate --test pool
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-egress-impersonate/src/pool.rs crates/gw-egress-impersonate/tests/pool.rs
git commit -m "feat(gw-egress-impersonate): ClientPool with LRU + proxy URL normalization"
```

---

### Task 3.4: rquest error → `X-Egress-Error-Class` mapping

**Files:**
- Modify: `crates/gw-egress-impersonate/src/error.rs`
- Test: inline

- [ ] **Step 1: Write failing test (matrix style)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_strings_are_stable() {
        // We can't easily construct each rquest::Error variant; just assert the
        // returned class strings match what gw-proxy will pattern-match on.
        for c in [
            ErrorClass::Tls, ErrorClass::ConnectTimeout, ErrorClass::ConnectRefused,
            ErrorClass::ReadTimeout, ErrorClass::H2Protocol, ErrorClass::BodyTruncated,
            ErrorClass::ProxyError, ErrorClass::Panic, ErrorClass::Unknown,
        ] {
            assert!(!c.as_str().is_empty());
        }
    }
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-egress-impersonate error
```

- [ ] **Step 3: Implement**

`crates/gw-egress-impersonate/src/error.rs`:

```rust
//! Map rquest errors → `X-Egress-Error-Class` header values.
//! The gw-proxy response_filter pattern-matches these strings.

pub const HDR_CLASS:  &str = "X-Egress-Error-Class";
pub const HDR_DETAIL: &str = "X-Egress-Error-Detail";

#[derive(Clone, Copy, Debug)]
pub enum ErrorClass {
    Tls,
    ConnectTimeout,
    ConnectRefused,
    ReadTimeout,
    H2Protocol,
    BodyTruncated,
    ProxyError,
    Panic,
    Unknown,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::ConnectTimeout => "connect_timeout",
            Self::ConnectRefused => "connect_refused",
            Self::ReadTimeout => "read_timeout",
            Self::H2Protocol => "h2_protocol",
            Self::BodyTruncated => "body_truncated",
            Self::ProxyError => "proxy_error",
            Self::Panic => "panic",
            Self::Unknown => "unknown",
        }
    }

    pub fn status_code(self) -> u16 {
        match self {
            Self::ConnectTimeout | Self::ReadTimeout => 504,
            _ => 502,
        }
    }
}

pub fn classify(err: &rquest::Error) -> ErrorClass {
    // rquest::Error doesn't expose a clean discriminant; categorize by Display
    // + the helpful predicates it does expose. Update as rquest grows the API.
    if err.is_timeout() { return ErrorClass::ConnectTimeout; }
    if err.is_connect() {
        let s = err.to_string();
        if s.contains("tls") || s.contains("certificate") || s.contains("handshake") {
            return ErrorClass::Tls;
        }
        if s.contains("proxy") { return ErrorClass::ProxyError; }
        return ErrorClass::ConnectRefused;
    }
    if err.is_body() { return ErrorClass::BodyTruncated; }
    if err.is_decode() { return ErrorClass::H2Protocol; }
    ErrorClass::Unknown
}
```

> `rquest::Error` helper predicates (`is_timeout`, `is_connect`, `is_body`, `is_decode`) inherited from reqwest. Confirm names against `cargo doc -p rquest`; if rquest renames them, adjust.

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-egress-impersonate error
```

- [ ] **Step 5: Commit**

```bash
git add crates/gw-egress-impersonate/src/error.rs
git commit -m "feat(gw-egress-impersonate): rquest error → ErrorClass mapping + HDR consts"
```

---

### Task 3.5: Egress request handler (`handle`)

**Files:**
- Modify: `crates/gw-egress-impersonate/src/server.rs`
- Test: `crates/gw-egress-impersonate/tests/handle.rs` (uses a hyper-test mock upstream)

- [ ] **Step 1: Write failing integration test**

`crates/gw-egress-impersonate/tests/handle.rs`:

```rust
use gw_egress_impersonate::server::Handler;
use gw_egress_impersonate::pool::ClientPool;
use std::sync::Arc;

#[tokio::test]
async fn handle_strips_internal_headers_before_upstream() {
    // 1. Spin up a mock upstream that captures incoming headers.
    // 2. Build an egress Handler with our ClientPool.
    // 3. Invoke handle(req) with X-Egress-Target/Profile + a dummy body.
    // 4. Assert mock upstream received no X-Egress-* headers + did receive Authorization.
    let captured = mock_upstream::spawn().await;

    let pool = Arc::new(ClientPool::new(4));
    let handler = Handler::new(pool);

    let req = hyper::Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("X-Egress-Target", format!("127.0.0.1:{}", captured.port))
        .header("X-Egress-Profile", "claude_code_chrome131")
        .header("Authorization", "Bearer test-token")
        .header("User-Agent", "Mozilla/5.0 (forged)")
        .body(http_body_util::Full::new(bytes::Bytes::from(r#"{"x":1}"#)))
        .unwrap();

    let _resp = handler.handle(req).await.expect("handle ok");

    let recv = captured.latest_headers().await;
    assert!(!recv.contains_key("X-Egress-Target"));
    assert!(!recv.contains_key("X-Egress-Profile"));
    assert_eq!(recv.get("Authorization").unwrap(), "Bearer test-token");
    assert_eq!(recv.get("User-Agent").unwrap(), "Mozilla/5.0 (forged)");
}

mod mock_upstream {
    //! Minimal hyper 1.x server that records the headers of the most
    //! recent request and replies with empty 200. Reused across egress tests.
    use http_body_util::{BodyExt, Empty};
    use hyper::body::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper::{HeaderMap, Request, Response};
    use hyper_util::rt::TokioIo;
    use parking_lot::Mutex;
    use std::convert::Infallible;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    pub struct Captured {
        pub port: u16,
        shared: Arc<Mutex<HeaderMap>>,
    }

    impl Captured {
        pub async fn latest_headers(&self) -> HeaderMap {
            self.shared.lock().clone()
        }
    }

    pub async fn spawn() -> Captured {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared: Arc<Mutex<HeaderMap>> = Arc::new(Mutex::new(HeaderMap::new()));
        let shared_for_loop = Arc::clone(&shared);
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v, Err(_) => continue,
                };
                let io = TokioIo::new(stream);
                let shared_for_conn = Arc::clone(&shared_for_loop);
                tokio::spawn(async move {
                    let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                        let shared = Arc::clone(&shared_for_conn);
                        async move {
                            *shared.lock() = req.headers().clone();
                            // Drain body so client write completes.
                            let _ = req.into_body().collect().await;
                            Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new()))
                        }
                    });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        });
        Captured { port, shared }
    }
}
```

> The mock upstream is shared infrastructure — implement it cleanly the first time; subsequent egress tests will reuse it. Consider moving it into `crates/gw-egress-impersonate/tests/common/mod.rs`.

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-egress-impersonate --test handle
```

- [ ] **Step 3: Implement Handler**

`crates/gw-egress-impersonate/src/server.rs`:

```rust
//! Request handler — strip internal headers, dispatch via rquest, stream back.

use crate::error::{classify, ErrorClass, HDR_CLASS, HDR_DETAIL};
use crate::pool::{ClientPool, PoolKey};
use bytes::Bytes;
use gw_core::impersonate::ImpersonateProfile;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use std::str::FromStr;
use std::sync::Arc;
use url::Url;

const INTERNAL_HEADERS: &[&str] = &[
    "x-egress-target",
    "x-egress-profile",
    "x-egress-proxy",
];

const HOP_BY_HOP: &[&str] = &[
    "connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
    "te", "trailers", "transfer-encoding", "upgrade", "host", "content-length",
];

pub struct Handler {
    pool: Arc<ClientPool>,
}

impl Handler {
    pub fn new(pool: Arc<ClientPool>) -> Self { Self { pool } }

    pub async fn handle<B>(
        &self, req: Request<B>,
    ) -> anyhow::Result<Response<hyper::body::Incoming>>
    where
        B: hyper::body::Body<Data = Bytes, Error = hyper::Error> + Send + 'static + Unpin,
    {
        let (parts, body) = req.into_parts();

        // 1. Extract internal headers.
        let target = parts.headers.get("X-Egress-Target")
            .ok_or_else(|| anyhow::anyhow!("missing X-Egress-Target"))?
            .to_str()?.to_owned();
        let profile_str = parts.headers.get("X-Egress-Profile")
            .ok_or_else(|| anyhow::anyhow!("missing X-Egress-Profile"))?
            .to_str()?.to_owned();
        let profile = ImpersonateProfile::from_str(&profile_str)
            .map_err(anyhow::Error::msg)?;
        let proxy = parts.headers.get("X-Egress-Proxy")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Url::parse(s).ok());

        // 2. Get/create rquest Client.
        let client = self.pool.get_or_create(PoolKey { profile, proxy }).await?;

        // 3. Rebuild URL: scheme=https + target + original path/query.
        let path_q = parts.uri.path_and_query()
            .map(|p| p.as_str()).unwrap_or("/");
        let url = format!("https://{}{}", target, path_q);

        // 4. Build rquest request: copy method + non-internal/non-hop headers.
        let mut rreq = client.request(parts.method.clone(), &url);
        for (k, v) in parts.headers.iter() {
            let name = k.as_str().to_ascii_lowercase();
            if INTERNAL_HEADERS.contains(&name.as_str()) { continue; }
            if HOP_BY_HOP.contains(&name.as_str()) { continue; }
            rreq = rreq.header(k.as_str(), v.as_bytes());
        }

        // 5. Stream the request body into rquest.
        //    Convert hyper Body → impl Stream<Item = Result<Bytes, _>>.
        let body_stream = http_body_util::BodyStream::new(body)
            .map_err(std::io::Error::other);   // adjust to rquest::Body::wrap_stream expectations
        let rreq = rreq.body(rquest::Body::wrap_stream(body_stream));

        // 6. Send. On error → synthetic response with X-Egress-Error-Class.
        let rresp = match rreq.send().await {
            Ok(r) => r,
            Err(e) => return Ok(error_response(classify(&e), &e.to_string())),
        };

        // 7. Map rquest response → hyper Response with streaming body.
        let status = rresp.status();
        let resp_headers = rresp.headers().clone();
        let body_stream = rresp.bytes_stream();
        let body = hyper::body::Incoming::from_stream(body_stream);  // see rquest streaming docs
        let mut builder = Response::builder().status(status);
        for (k, v) in resp_headers.iter() {
            let name = k.as_str().to_ascii_lowercase();
            if HOP_BY_HOP.contains(&name.as_str()) { continue; }
            builder = builder.header(k.as_str(), v.as_bytes());
        }
        Ok(builder.body(body)?)
    }
}

fn error_response(class: ErrorClass, detail: &str) -> Response<hyper::body::Incoming> {
    let truncated: String = detail.chars().take(256).collect();
    Response::builder()
        .status(StatusCode::from_u16(class.status_code()).unwrap())
        .header(HDR_CLASS, class.as_str())
        .header(HDR_DETAIL, truncated)
        .body(hyper::body::Incoming::empty())   // adjust per hyper 1.x API
        .unwrap()
}
```

> The `hyper::body::Incoming::from_stream` / `::empty` exact names depend on which hyper helper crate you're using. The intent is clear; adjust API surface accordingly. Use `http_body_util::Empty` / `Full` / `StreamBody` as needed.

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-egress-impersonate --test handle
```

Expected: pass (mock upstream sees no `X-Egress-*` but does see `Authorization`/`User-Agent`).

- [ ] **Step 5: Commit**

```bash
git add crates/gw-egress-impersonate/src/server.rs \
        crates/gw-egress-impersonate/tests/handle.rs
git commit -m "feat(gw-egress-impersonate): request Handler — strip internal headers + dispatch via rquest"
```

---

### Task 3.6: UDS / TCP listener (`Listener`)

**Files:**
- Modify: `crates/gw-egress-impersonate/src/server.rs` (add `Listener`)
- Test: `crates/gw-egress-impersonate/tests/listener_uds.rs`

- [ ] **Step 1: Write failing test (UDS path)**

```rust
use gw_egress_impersonate::handle::EgressBind;
use gw_egress_impersonate::server::Listener;
use gw_egress_impersonate::pool::ClientPool;
use std::sync::Arc;

#[tokio::test]
async fn uds_listener_accepts_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("egress.sock");

    let mock = super::mock_upstream::spawn().await;  // assume re-exported from common
    let pool = Arc::new(ClientPool::new(4));
    let listener = Listener::bind(EgressBind::Uds(sock.clone()), pool).await.unwrap();
    let handle = listener.spawn();

    // Make an HTTP/1.1 connection over UDS using hyper-util.
    let stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    // ... send a request through hyper::client over the UnixStream pointing at mock.

    assert!(mock.latest_headers().await.contains_key("User-Agent"));
    handle.shutdown().await;
}
```

- [ ] **Step 2: Run, verify failure**

```bash
cargo test -p gw-egress-impersonate --test listener_uds
```

- [ ] **Step 3: Implement**

Append to `server.rs`:

```rust
use crate::handle::EgressBind;
use std::path::PathBuf;
use tokio::net::{TcpListener, UnixListener};

pub struct Listener {
    bind: EgressBind,
    handler: Arc<Handler>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

impl Listener {
    pub async fn bind(bind: EgressBind, pool: Arc<ClientPool>) -> anyhow::Result<Self> {
        // Pre-cleanup for UDS path collisions.
        if let EgressBind::Uds(ref p) = bind {
            if p.exists() { std::fs::remove_file(p)?; }
        }
        let (tx, rx) = tokio::sync::watch::channel(false);
        Ok(Self {
            bind,
            handler: Arc::new(Handler::new(pool)),
            shutdown_tx: tx,
            shutdown_rx: rx,
        })
    }

    pub fn spawn(self) -> ListenerJoinHandle {
        let bind = self.bind.clone();
        let handler = Arc::clone(&self.handler);
        let mut shutdown_rx = self.shutdown_rx.clone();
        let tx = self.shutdown_tx.clone();

        let task = tokio::spawn(async move {
            match bind {
                EgressBind::Uds(path) => {
                    let listener = UnixListener::bind(&path)?;
                    tracing::info!("gw-egress-impersonate UDS bound at {:?}", path);
                    loop {
                        tokio::select! {
                            _ = shutdown_rx.changed() => break,
                            accept = listener.accept() => match accept {
                                Ok((stream, _)) => {
                                    let handler = Arc::clone(&handler);
                                    tokio::spawn(serve_conn_unix(stream, handler));
                                }
                                Err(e) => tracing::warn!("UDS accept err: {e}"),
                            }
                        }
                    }
                }
                EgressBind::Tcp(addr) => {
                    let listener = TcpListener::bind(addr).await?;
                    tracing::info!("gw-egress-impersonate TCP bound at {}", listener.local_addr()?);
                    loop {
                        tokio::select! {
                            _ = shutdown_rx.changed() => break,
                            accept = listener.accept() => match accept {
                                Ok((stream, peer)) => {
                                    if !peer.ip().is_loopback() {
                                        tracing::warn!("rejecting non-loopback peer {peer}");
                                        continue;
                                    }
                                    let handler = Arc::clone(&handler);
                                    tokio::spawn(serve_conn_tcp(stream, handler));
                                }
                                Err(e) => tracing::warn!("TCP accept err: {e}"),
                            }
                        }
                    }
                }
            }
            anyhow::Result::<()>::Ok(())
        });
        ListenerJoinHandle { task, shutdown_tx: tx }
    }
}

pub struct ListenerJoinHandle {
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl ListenerJoinHandle {
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.task.await;
    }
}

async fn serve_conn_unix(stream: tokio::net::UnixStream, handler: Arc<Handler>) {
    let io = hyper_util::rt::TokioIo::new(stream);
    let service = hyper::service::service_fn(move |req| {
        let h = Arc::clone(&handler);
        async move { h.handle(req).await }
    });
    if let Err(e) = hyper::server::conn::http1::Builder::new().serve_connection(io, service).await {
        tracing::debug!("conn err (uds): {e}");
    }
}

async fn serve_conn_tcp(stream: tokio::net::TcpStream, handler: Arc<Handler>) {
    let io = hyper_util::rt::TokioIo::new(stream);
    let service = hyper::service::service_fn(move |req| {
        let h = Arc::clone(&handler);
        async move { h.handle(req).await }
    });
    if let Err(e) = hyper::server::conn::http1::Builder::new().serve_connection(io, service).await {
        tracing::debug!("conn err (tcp): {e}");
    }
}
```

- [ ] **Step 4: Run, verify pass**

```bash
cargo test -p gw-egress-impersonate --test listener_uds
```

- [ ] **Step 5: Commit**

```bash
git add crates/gw-egress-impersonate/src/server.rs \
        crates/gw-egress-impersonate/tests/listener_uds.rs
git commit -m "feat(gw-egress-impersonate): Listener for UDS + TCP loopback, graceful shutdown"
```

---

### Task 3.7: Supervisor + restart on panic

**Files:**
- Modify: `crates/gw-egress-impersonate/src/supervisor.rs`
- Test: skipped (panic-restart loop is awkward to unit test; covered by manual smoke in Phase 6).

- [ ] **Step 1: Implement**

```rust
//! Supervisor — wraps `Listener::spawn` with catch_unwind + exponential backoff
//! restart. Caps at 5 restarts per minute; beyond that, logs critical and exits
//! the supervisor task (process keeps running, egress is down → impersonation
//! requests will fail with connect_refused, which gw-proxy treats as retryable).

use crate::handle::EgressBind;
use crate::pool::ClientPool;
use crate::server::Listener;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub async fn supervise(bind: EgressBind, pool: Arc<ClientPool>) {
    let mut restarts: Vec<Instant> = Vec::new();
    loop {
        let listener = match Listener::bind(bind.clone(), Arc::clone(&pool)).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("egress listener bind failed: {e}");
                metrics::counter!("gw_egress_supervisor_restart_total", "reason" => "bind_failed").increment(1);
                tokio::time::sleep(backoff(restarts.len())).await;
                continue;
            }
        };
        let handle = listener.spawn();
        let result = handle.task.await;  // wait for task end
        match result {
            Ok(Ok(())) => break,           // graceful shutdown
            Ok(Err(e)) => tracing::warn!("egress listener exited with err: {e}"),
            Err(e) if e.is_panic() => tracing::error!("egress listener panicked: {e:?}"),
            Err(e) => tracing::warn!("egress listener join err: {e}"),
        }

        let now = Instant::now();
        restarts.retain(|t| now.duration_since(*t) <= Duration::from_secs(60));
        restarts.push(now);
        if restarts.len() > 5 {
            tracing::error!("egress supervisor: >5 restarts/min, giving up");
            metrics::counter!("gw_egress_supervisor_restart_total", "reason" => "exhausted").increment(1);
            break;
        }
        metrics::counter!("gw_egress_supervisor_restart_total", "reason" => "panic_or_err").increment(1);
        tokio::time::sleep(backoff(restarts.len())).await;
    }
}

fn backoff(restart_count: usize) -> Duration {
    let secs = match restart_count {
        0 => 1, 1 => 2, 2 => 4, 3 => 8, _ => 16,
    };
    Duration::from_secs(secs)
}
```

The `ListenerJoinHandle::task` field needs `pub(crate)` visibility — adjust in `server.rs`.

- [ ] **Step 2: Build check**

```bash
cargo check -p gw-egress-impersonate
```

- [ ] **Step 3: Commit**

```bash
git add crates/gw-egress-impersonate/src/supervisor.rs \
        crates/gw-egress-impersonate/src/server.rs
git commit -m "feat(gw-egress-impersonate): supervisor with capped restart (5/min)"
```

---

### Task 3.8: `EgressHandle` public API

**Files:**
- Modify: `crates/gw-egress-impersonate/src/handle.rs`

- [ ] **Step 1: Implement**

```rust
//! Public handle the gateway holds in its `ProxyHttp` impl.

use crate::pool::ClientPool;
use crate::supervisor::supervise;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub enum EgressBind {
    Uds(PathBuf),
    Tcp(SocketAddr),
}

#[derive(Clone)]
pub struct EgressHandle {
    bind: EgressBind,
    shutdown_tx: broadcast::Sender<()>,
}

impl EgressHandle {
    /// Spawn the supervisor + listener. Returns a handle the gateway holds.
    pub async fn start(bind: EgressBind, max_sessions: usize) -> Self {
        let pool = Arc::new(ClientPool::new(max_sessions));
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let bind_clone = bind.clone();
        tokio::spawn(async move {
            supervise(bind_clone, pool, shutdown_rx).await;
        });
        Self { bind, shutdown_tx }
    }

    pub fn bind_addr(&self) -> &EgressBind { &self.bind }

    /// Cooperative shutdown — signals the supervisor to stop accepting and
    /// waits up to `drain_timeout` for inflight egress connections to drain.
    pub async fn shutdown(self, drain_timeout: Duration) {
        let _ = self.shutdown_tx.send(());
        // Best-effort drain: yield repeatedly until either the supervisor's
        // accept loops exit (subscriber count drops to zero) or timeout fires.
        let deadline = tokio::time::Instant::now() + drain_timeout;
        loop {
            if self.shutdown_tx.receiver_count() == 0 { break; }
            if tokio::time::Instant::now() >= deadline { break; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
```

The supervisor signature changes accordingly — update `crates/gw-egress-impersonate/src/supervisor.rs::supervise`:

```rust
pub async fn supervise(
    bind: EgressBind,
    pool: Arc<ClientPool>,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let mut restarts: Vec<Instant> = Vec::new();
    loop {
        // Bail if shutdown was requested before we even start a listener.
        if shutdown_rx.try_recv().is_ok() { break; }

        let listener = match Listener::bind(bind.clone(), Arc::clone(&pool)).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("egress listener bind failed: {e}");
                metrics::counter!("gw_egress_supervisor_restart_total", "reason" => "bind_failed").increment(1);
                tokio::time::sleep(backoff(restarts.len())).await;
                continue;
            }
        };
        let handle = listener.spawn();
        // Race the supervisor's shutdown signal against the listener task.
        tokio::select! {
            res = &mut handle.task => {
                match res {
                    Ok(Ok(())) => break,
                    Ok(Err(e)) => tracing::warn!("egress listener exited with err: {e}"),
                    Err(e) if e.is_panic() => tracing::error!("egress listener panicked: {e:?}"),
                    Err(e) => tracing::warn!("egress listener join err: {e}"),
                }
            }
            _ = shutdown_rx.recv() => {
                handle.shutdown().await;
                break;
            }
        }

        let now = Instant::now();
        restarts.retain(|t| now.duration_since(*t) <= Duration::from_secs(60));
        restarts.push(now);
        if restarts.len() > 5 {
            tracing::error!("egress supervisor: >5 restarts/min, giving up");
            metrics::counter!("gw_egress_supervisor_restart_total", "reason" => "exhausted").increment(1);
            break;
        }
        metrics::counter!("gw_egress_supervisor_restart_total", "reason" => "panic_or_err").increment(1);
        tokio::time::sleep(backoff(restarts.len())).await;
    }
}
```

And `gw-gateway::main` shutdown wires it up:

```rust
// At graceful shutdown:
egress.shutdown(std::time::Duration::from_secs(15)).await;
```

- [ ] **Step 2: Build check**

```bash
cargo build -p gw-egress-impersonate
```

- [ ] **Step 3: Commit**

```bash
git add crates/gw-egress-impersonate/src/handle.rs crates/gw-egress-impersonate/src/lib.rs
git commit -m "feat(gw-egress-impersonate): EgressHandle public API + start/shutdown"
```

---

### Task 3.9: Phase 3 regression check

- [ ] **Step 1: Run workspace tests + Phase 0 baseline**

```bash
cargo test --workspace --no-fail-fast
cargo test -p gw-proxy --test regression_rustls_baseline
```

Expected: all green. The new crate runs its tests; everything else unchanged.

- [ ] **Step 2: Phase marker commit**

```bash
git add -u && git commit -m "chore: end of Phase 3 — gw-egress-impersonate crate complete" || true
```

---

## Phase 4 — Gateway Wiring (`gw-proxy` + `gw-gateway`)

> The first phase that introduces user-visible behavior changes (for vendors opted into Impersonate).

### Task 4.1: Add fields to `ReqCtx`

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs::ReqCtx`

- [ ] **Step 1: Implement (no test — pure scaffolding consumed in 4.2/4.3)**

Add to `ReqCtx`:

```rust
    /// Set in `pick_peer` when `Vendor.egress_mode == Impersonate{..}`.
    /// Consumed in `inject_vendor_headers` to write X-Egress-* + identity.
    pub egress_profile: Option<gw_core::impersonate::ImpersonateProfile>,
    /// Real upstream (host, port) — communicated to the loopback egress via
    /// `X-Egress-Target`.
    pub egress_upstream: Option<(String, u16)>,
```

Initialize them in `Default for ReqCtx` (or wherever `ReqCtx` is constructed):

```rust
            egress_profile: None,
            egress_upstream: None,
```

- [ ] **Step 2: Build check**

```bash
cargo build -p gw-proxy
```

- [ ] **Step 3: Commit**

```bash
git add crates/gw-proxy/src/ctx.rs
git commit -m "refactor(gw-proxy): add ReqCtx.egress_profile + egress_upstream"
```

---

### Task 4.2: `pick_peer` — Impersonate branch + tunnel exclusion

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs::pick_peer` (~line 309-385)
- Modify: `crates/gw-proxy/src/gateway.rs::Proxy` constructor (accept `EgressHandle`)
- Modify: `crates/gw-core/src/error/codes.rs` — add `GW_TUNNEL_IMPERSONATE_CONFLICT`

- [ ] **Step 1: Write failing integration test**

`crates/gw-proxy/tests/pick_peer_impersonate.rs`:

```rust
// Integration test stub — full behavior covered by Phase 6's e2e_impersonate.
// Here we just smoke-test the branch selection:
// 1. Build a fake Vendor with egress_mode = Impersonate{ClaudeCodeChrome131}.
// 2. Verify that pick_peer constructs HttpPeer with tls=false and addr=egress.

// (Engineer: use Proxy::for_test() or whatever existing scaffold the gw-proxy
//  tests use. If none exists, this test can be deferred to Phase 6 and only
//  Step 3's code change committed without a new test file.)
```

If no clean test harness exists, skip the test for this task and rely on Phase 6 e2e coverage. **Lock the manual smoke verification** into the commit message.

- [ ] **Step 2: Add error code**

Modify `crates/gw-core/src/error/codes.rs` — add:

```rust
pub const GW_TUNNEL_IMPERSONATE_CONFLICT: &str = "GW_TUNNEL_IMPERSONATE_CONFLICT";
```

- [ ] **Step 3: Implement `pick_peer` change**

In `crates/gw-proxy/src/filters/upstream.rs::pick_peer`, locate the section starting `let mut peer;` (around line 309 — the `#[cfg(feature = "tunnel")]` block) and the fallback `#[cfg(not(feature = "tunnel"))]` block. Restructure to:

```rust
use gw_core::impersonate::VendorEgressMode;

// (existing tunnel resolution code yields `resolution: TunnelResolution`
//  inside the cfg(tunnel) block — call its outcome `tunnel_use_node`.)

let impersonate_profile = match vendor.value.egress_mode {
    VendorEgressMode::Impersonate { profile } => Some(profile),
    VendorEgressMode::Rustls => None,
};

// Hard exclusion: a vendor cannot tunnel AND impersonate.
#[cfg(feature = "tunnel")]
if impersonate_profile.is_some() {
    if let gw_tunnel_client::resolver::TunnelResolution::Use(_) = &resolution {
        return emit_envelope_and_bail(
            session, ctx,
            envelope_for(
                codes::GW_TUNNEL_IMPERSONATE_CONFLICT,
                "vendor cannot enable both tunnel and impersonate",
                503,
            ),
        ).await;
    }
}

let mut peer = if let Some(profile) = impersonate_profile {
    ctx.egress_profile = Some(profile);
    ctx.egress_upstream = Some((host.clone(), port));
    match self.egress.bind_addr() {
        gw_egress_impersonate::EgressBind::Uds(p) => {
            HttpPeer::new_uds(p.to_string_lossy().as_ref(), false, host.clone())
        }
        gw_egress_impersonate::EgressBind::Tcp(addr) => {
            HttpPeer::new(addr.to_string(), false, host.clone())
        }
    }
} else {
    // Existing tunnel-or-direct path (verbatim from current code).
    #[cfg(feature = "tunnel")]
    {
        match resolution { /* existing match: Direct → HttpPeer; Use(node) → tunnel.peer_for_node */ }
    }
    #[cfg(not(feature = "tunnel"))]
    {
        ctx.tunnel_resolution = Some("direct".into());
        HttpPeer::new(format!("{host}:{port}"), scheme_tls, host.clone())
    }
};
```

The `self.egress` is `Arc<EgressHandle>`; thread it through:

In `crates/gw-proxy/src/gateway.rs::Proxy`:

```rust
pub struct Proxy {
    // existing fields...
    pub egress: Arc<gw_egress_impersonate::EgressHandle>,
}

// Constructor signature picks up egress: ...
```

And `pick_peer` already gets `&self`, but `filters::upstream::pick_peer` is a free function — pass `&Arc<EgressHandle>` as an additional parameter:

```rust
pub async fn pick_peer(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    health: &Arc<KeyHealthStore>,
    in_flight: &Arc<InFlightStore>,
    strategies: &Arc<StrategySet>,
    latency_tracker: &Arc<LatencyTracker>,
    egress: &Arc<gw_egress_impersonate::EgressHandle>,        // NEW
    #[cfg(feature = "tunnel")] tunnel: Option<&Arc<crate::tunnel_stack::TunnelStack>>,
) -> Result<Box<HttpPeer>> { ... }
```

Update the call site in `gateway.rs::upstream_peer` to pass `&self.egress`.

Add `gw-egress-impersonate = { path = "../gw-egress-impersonate" }` to `crates/gw-proxy/Cargo.toml`.

- [ ] **Step 4: Run, verify build + Phase 0 baseline**

```bash
cargo build --workspace
cargo test -p gw-proxy --test regression_rustls_baseline
```

Expected: builds cleanly; baseline still passes (Vendor without `egress_mode` set → Rustls branch → bytes unchanged).

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/filters/upstream.rs \
        crates/gw-proxy/src/gateway.rs \
        crates/gw-proxy/Cargo.toml \
        crates/gw-core/src/error/codes.rs \
        crates/gw-proxy/tests/pick_peer_impersonate.rs
git commit -m "feat(gw-proxy): pick_peer Impersonate branch + tunnel exclusion check"
```

---

### Task 4.3: `inject_vendor_headers` — write X-Egress-* + identity headers

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs::inject_vendor_headers` (~line 854)

- [ ] **Step 1: Implement**

At the end of `inject_vendor_headers` (after the existing `traceparent` / `content-length` writes), add:

```rust
    if let Some(profile) = ctx.egress_profile {
        let (target_host, target_port) = ctx.egress_upstream.as_ref()
            .expect("egress_upstream set when egress_profile is");
        let _ = upstream_request.insert_header("X-Egress-Target",
            format!("{target_host}:{target_port}"));
        let _ = upstream_request.insert_header("X-Egress-Profile",
            profile.as_static_str());
        if let Some(proxy) = picked.proxy.as_ref() {
            let _ = upstream_request.insert_header("X-Egress-Proxy", proxy.as_str());
        }

        // Derive + (maybe) persist per-VendorKey identity.
        let (resolved, persist) = gw_core::impersonate::ensure_identity(
            picked.key_id.as_ref(),
            picked.identity_snapshot.as_ref(),
            profile,
        );
        if persist {
            // Caller wires the persist queue via ctx (Task 4.7 hooks it).
            if let Some(q) = ctx.identity_persist.as_ref() {
                let snap = gw_core::impersonate::IdentitySnapshot {
                    profile_kind: resolved.profile,
                    variable: resolved.variable.clone(),
                };
                q.schedule(picked.key_id.as_ref().to_owned(), snap);
            }
        }

        let identity_headers = identity_headers_for(
            &resolved, ctx, picked.as_ref()
        ).await;
        for (name, value) in identity_headers {
            let _ = upstream_request.insert_header(name, value);
        }
    }
```

Add helper `identity_headers_for`:

```rust
use gw_core::impersonate::{claude_code, gemini_cli, openai_codex, ImpersonateProfile, IdentityVariable};

async fn identity_headers_for(
    resolved: &gw_core::impersonate::ResolvedIdentity,
    ctx: &ReqCtx,
    picked: &VendorKey,
) -> Vec<(String, String)> {
    match resolved.profile {
        ImpersonateProfile::ClaudeCodeChrome131 => {
            claude_code::headers(&resolved.variable, ctx.is_stream)
        }
        ImpersonateProfile::OpenAiCodex => {
            use gw_core::impersonate::openai_codex::PerRequest;
            let turn_state = ctx.turn_state_cache.as_ref()
                .expect("turn_state_cache present when ctx.egress_profile == OpenAiCodex")
                .get_or_create(
                    picked.key_id.as_ref(),
                    ctx.conversation_id.as_deref(),
                ).await;
            let per_req = PerRequest {
                session_id: uuid::Uuid::new_v4(),
                thread_id: uuid::Uuid::new_v4(),
                window_id: uuid::Uuid::new_v4(),
                turn_state,
            };
            openai_codex::headers(&resolved.variable, ctx.is_stream, &per_req)
        }
        ImpersonateProfile::GeminiCli => {
            let model = ctx.model.as_ref().map(|m| m.as_ref()).unwrap_or("");
            let quota = picked.oauth.as_ref().and_then(|o| o.quota_project_id.as_deref());
            gemini_cli::headers(&resolved.variable, model, quota)
        }
    }
}
```

Update `ReqCtx` to carry the queue + cache references (Arc-wrapped, set at gateway start):

```rust
// In ReqCtx:
pub identity_persist: Option<Arc<gw_store::identity_persist::IdentityPersistQueue>>,
pub turn_state_cache: Option<Arc<gw_core::impersonate::turn_state::TurnStateCache>>,
```

- [ ] **Step 2: Build check + baseline**

```bash
cargo build --workspace
cargo test -p gw-proxy --test regression_rustls_baseline
```

Expected: clean; baseline passes (only fires when `ctx.egress_profile.is_some()`, which is only set for Impersonate vendors).

- [ ] **Step 3: Commit**

```bash
git add crates/gw-proxy/src/filters/upstream.rs crates/gw-proxy/src/ctx.rs
git commit -m "feat(gw-proxy): inject_vendor_headers — write X-Egress-* + forged identity"
```

---

### Task 4.4: `response_filter` — read `X-Egress-Error-Class`, integrate with retry/blacklist

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-core/src/error/codes.rs` — add new error codes

- [ ] **Step 1: Add error codes**

`crates/gw-core/src/error/codes.rs`:

```rust
pub const GW_UPSTREAM_TLS_ERROR: &str         = "GW_UPSTREAM_TLS_ERROR";
pub const GW_UPSTREAM_CONNECT_TIMEOUT: &str   = "GW_UPSTREAM_CONNECT_TIMEOUT";
pub const GW_UPSTREAM_CONNECT_REFUSED: &str   = "GW_UPSTREAM_CONNECT_REFUSED";
pub const GW_UPSTREAM_READ_TIMEOUT: &str      = "GW_UPSTREAM_READ_TIMEOUT";
pub const GW_UPSTREAM_H2_PROTOCOL: &str       = "GW_UPSTREAM_H2_PROTOCOL";
pub const GW_UPSTREAM_BODY_TRUNCATED: &str    = "GW_UPSTREAM_BODY_TRUNCATED";
pub const GW_UPSTREAM_PROXY_ERROR: &str       = "GW_UPSTREAM_PROXY_ERROR";
pub const GW_UPSTREAM_PANIC: &str             = "GW_UPSTREAM_PANIC";
pub const GW_UPSTREAM_UNKNOWN: &str           = "GW_UPSTREAM_UNKNOWN";
```

- [ ] **Step 2: Implement response_filter handling**

In `crates/gw-proxy/src/filters/response.rs` (or wherever `response_filter` lives — check `gateway.rs:582`), add at the top of the function body:

```rust
const HDR_CLASS: &str = "X-Egress-Error-Class";
const HDR_DETAIL: &str = "X-Egress-Error-Detail";

if let Some(class) = upstream_response.headers.get(HDR_CLASS).and_then(|v| v.to_str().ok()) {
    let (code, retryable, is_tls) = map_egress_class(class);
    ctx.upstream_error_code = Some(code);
    // Capture detail for error_emit's upstream_raw field.
    if let Some(detail) = upstream_response.headers.get(HDR_DETAIL).and_then(|v| v.to_str().ok()) {
        ctx.upstream_error_detail = Some(detail.to_owned());
    }
    if is_tls {
        metrics::counter!("gw_retry_attempt_total", "outcome" => "tls_blocked").increment(1);
        return Err(pingora::Error::new_str("gw_upstream_tls_blocked"));
    }
    if retryable {
        if let Some(picked) = ctx.picked_vendor_key.as_ref() {
            ctx.retry_blacklist.push(picked.key_id.clone());
        }
        // Trigger Pingora's retry → next pick_peer lands on a different key.
        return Err(pingora::Error::new_str("gw_egress_retry"));
    }
    // Non-retryable → let the error propagate to error_emit with upstream_raw.
}

// Always strip internal headers before client sees them.
let _ = upstream_response.remove_header("X-Egress-Error-Class");
let _ = upstream_response.remove_header("X-Egress-Error-Detail");
```

Add the mapping function next to it:

```rust
fn map_egress_class(class: &str) -> (&'static str, bool, bool) {
    use gw_core::error::codes::*;
    match class {
        "tls"             => (GW_UPSTREAM_TLS_ERROR,       false, true),
        "connect_timeout" => (GW_UPSTREAM_CONNECT_TIMEOUT, true,  false),
        "connect_refused" => (GW_UPSTREAM_CONNECT_REFUSED, true,  false),
        "read_timeout"    => (GW_UPSTREAM_READ_TIMEOUT,    true,  false),
        "h2_protocol"     => (GW_UPSTREAM_H2_PROTOCOL,     true,  false),
        "body_truncated"  => (GW_UPSTREAM_BODY_TRUNCATED,  false, false),
        "proxy_error"     => (GW_UPSTREAM_PROXY_ERROR,     true,  false),
        "panic"           => (GW_UPSTREAM_PANIC,           false, false),
        _                 => (GW_UPSTREAM_UNKNOWN,         false, false),
    }
}
```

Add `ctx.upstream_error_detail: Option<String>` to `ReqCtx` if not present.

> The exact retry-trigger mechanism in Pingora 0.8 depends on whether the project uses `failover` from an outer loop or relies on Pingora's `fail_to_proxy` retry. Inspect existing retry paths (`gateway.rs:817-848` already shows the pattern for fail_to_connect TLS handling) and mirror its triggering mechanism. If the gateway has its own `FailoverExecutor`, surface the blacklist + return a retryable status code.

- [ ] **Step 3: Run baseline**

```bash
cargo build --workspace
cargo test -p gw-proxy --test regression_rustls_baseline
```

Expected: clean; baseline unchanged (no `X-Egress-Error-Class` ever present in rustls path).

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/filters/response.rs \
        crates/gw-proxy/src/ctx.rs \
        crates/gw-core/src/error/codes.rs
git commit -m "feat(gw-proxy): response_filter — map X-Egress-Error-Class to retry/blacklist"
```

---

### Task 4.5: `detailed_log` — extend strip list

**Files:**
- Modify: `crates/gw-proxy/src/detailed_log.rs`

- [ ] **Step 1: Find existing strip rules + extend**

Search for the existing redaction list (likely a `const SENSITIVE_HEADERS` or similar):

```bash
grep -nE "Authorization|x-api-key|redact|strip|SENSITIVE" crates/gw-proxy/src/detailed_log.rs
```

Add to it (case-insensitive matching):

```rust
const EGRESS_INTERNAL: &[&str] = &[
    "x-egress-target",
    "x-egress-profile",
    "x-egress-proxy",
    "x-egress-error-class",
    "x-egress-error-detail",
];
```

And in the iterator that writes archive headers, skip names whose lowercase form appears in `EGRESS_INTERNAL`.

- [ ] **Step 2: Write a test that archives a request with X-Egress-* headers and asserts they're absent in the archived form**

`crates/gw-proxy/tests/detailed_log_strip.rs`:

```rust
// Test the redaction in isolation — call detailed_log's archive fn with a
// fixture request that contains X-Egress-* headers, assert they don't appear
// in the captured archive value.
```

- [ ] **Step 3: Run, verify pass**

```bash
cargo test -p gw-proxy --test detailed_log_strip
```

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/detailed_log.rs crates/gw-proxy/tests/detailed_log_strip.rs
git commit -m "feat(gw-proxy): strip X-Egress-* from detailed_log archives"
```

---

### Task 4.6: `gw-gateway::main` — construct EgressHandle + persist queue + turn-state cache

**Files:**
- Modify: `crates/gw-gateway/src/main.rs` (or wherever `Proxy` is instantiated)

- [ ] **Step 1: Implement**

In `gw-gateway` startup:

```rust
use gw_egress_impersonate::{EgressBind, EgressHandle};
use gw_store::identity_persist::{IdentityPersistQueue, PgSnapshotWriter};
use gw_core::impersonate::turn_state::TurnStateCache;
use std::sync::Arc;
use std::time::Duration;

// Read config (see Task 5.x for figment integration).
let egress_bind = match cfg.egress.bind.as_deref().unwrap_or("uds:///tmp/gw-egress.sock") {
    s if s.starts_with("uds://") => EgressBind::Uds(s.strip_prefix("uds://").unwrap().into()),
    s if s.starts_with("tcp://") => {
        EgressBind::Tcp(s.strip_prefix("tcp://").unwrap().parse()?)
    }
    other => anyhow::bail!("bad egress.bind: {other}"),
};
let max_sessions = cfg.egress.max_sessions.unwrap_or(64);
let egress = Arc::new(EgressHandle::start(egress_bind, max_sessions).await);

let persist_queue = Arc::new(IdentityPersistQueue::new(
    Arc::new(PgSnapshotWriter::new(pg_pool.clone())),
    /*max_pending*/ 2000,
));

let turn_state_cache = Arc::new(TurnStateCache::new(
    Duration::from_secs(cfg.egress.turn_state_ttl_secs.unwrap_or(300)),
    /*capacity*/ 8192,
));

// Pass to Proxy::new(...). The Proxy stores them on `self` and sets them
// on ReqCtx in early_request_filter.
let proxy = Proxy::new(/* ..., */ egress.clone(), persist_queue.clone(), turn_state_cache.clone());
```

Edit `Proxy::new` accordingly. In `early_request_filter` (or wherever `ReqCtx` is constructed per request), populate:

```rust
ctx.identity_persist = Some(Arc::clone(&self.identity_persist));
ctx.turn_state_cache = Some(Arc::clone(&self.turn_state_cache));
```

- [ ] **Step 2: Build check**

```bash
cargo build --workspace
```

- [ ] **Step 3: Commit**

```bash
git add crates/gw-gateway/src/main.rs \
        crates/gw-proxy/src/gateway.rs \
        crates/gw-proxy/src/ctx.rs
git commit -m "feat(gw-gateway): construct EgressHandle + persist queue + turn-state cache"
```

---

### Task 4.7: Phase 4 regression check

- [ ] **Step 1: Re-run baseline + full suite**

```bash
cargo test -p gw-proxy --test regression_rustls_baseline
cargo test --workspace --no-fail-fast
```

Expected: all green. With no Vendor configured for Impersonate, behavior unchanged.

- [ ] **Step 2: Phase marker commit**

```bash
git commit --allow-empty -m "chore: end of Phase 4 — gateway wiring complete"
```

---

## Phase 5 — Build / Config / Docs

### Task 5.1: Dockerfile — add boring-sys system deps

**Files:**
- Modify: `Dockerfile`

- [ ] **Step 1: Add apt-get line above the cargo build step**

```dockerfile
# boring-sys (via rquest) build deps. Adds ~150MB to base image; cached by BuildKit.
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake perl pkg-config libclang-dev build-essential ca-certificates \
    && rm -rf /var/lib/apt/lists/*
```

- [ ] **Step 2: Verify image builds**

```bash
docker build -t ailinkmax-gateway:dev .
```

Expected: build succeeds; final image runnable.

- [ ] **Step 3: Commit**

```bash
git add Dockerfile
git commit -m "build: install boring-sys system deps (cmake/perl/libclang-dev)"
```

---

### Task 5.2: Config — `[egress]` section + figment loader

**Files:**
- Modify: `config/default.toml`
- Modify: `crates/gw-gateway/src/config.rs` (or wherever figment config is parsed)

- [ ] **Step 1: Add to `config/default.toml`**

```toml
[egress]
# UDS preferred for performance; falls back to TCP 127.0.0.1 if unsupported.
# Format: "uds:///path/to/sock" | "tcp://127.0.0.1:0"
bind = "uds:///tmp/gw-egress.sock"
# rquest Client pool cap (per profile × proxy).
max_sessions = 64
# rquest Client LRU idle eviction.
idle_timeout_secs = 300
# Emergency kill-switch: when true, pick_peer treats Impersonate vendors as Rustls.
disabled = false
# Codex turn_state Moka cache TTL.
turn_state_ttl_secs = 300
```

- [ ] **Step 2: Add config struct**

In `crates/gw-gateway/src/config.rs`:

```rust
#[derive(Clone, Debug, Deserialize, Default)]
pub struct EgressConfig {
    pub bind: Option<String>,
    pub max_sessions: Option<usize>,
    pub idle_timeout_secs: Option<u64>,
    pub disabled: Option<bool>,
    pub turn_state_ttl_secs: Option<u64>,
}

// Add to the main Config struct:
#[derive(Clone, Debug, Deserialize, Default)]
pub struct Config {
    // existing fields...
    #[serde(default)]
    pub egress: EgressConfig,
}
```

- [ ] **Step 3: Wire `cfg.egress.disabled` into `pick_peer`**

Back in `crates/gw-proxy/src/filters/upstream.rs::pick_peer`, change the impersonate selection to:

```rust
let impersonate_profile = match vendor.value.egress_mode {
    VendorEgressMode::Impersonate { profile } if !egress_disabled => Some(profile),
    VendorEgressMode::Impersonate { .. } => {
        metrics::counter!("gw_egress_disabled_total").increment(1);
        None
    }
    VendorEgressMode::Rustls => None,
};
```

Thread `egress_disabled: bool` through the function signature (read from `self.config.egress.disabled` at the call site in `upstream_peer`).

- [ ] **Step 4: Run baseline**

```bash
cargo build --workspace
cargo test -p gw-proxy --test regression_rustls_baseline
```

- [ ] **Step 5: Commit**

```bash
git add config/default.toml \
        crates/gw-gateway/src/config.rs \
        crates/gw-proxy/src/filters/upstream.rs \
        crates/gw-proxy/src/gateway.rs
git commit -m "feat(gw-gateway): [egress] config section + kill-switch (disabled=true)"
```

---

### Task 5.3: CLAUDE.md / architecture.md / workflow.md updates

**Files:**
- Modify: `CLAUDE.md`
- Modify: `docs/architecture.md`
- Modify: `docs/workflow.md`
- Create: `docs/issues/egress-impersonate-known-gaps.md`

- [ ] **Step 1: Update `CLAUDE.md`**

Add to the "source layout" section:

```markdown
- `crates/gw-egress-impersonate` — In-process BoringSSL/rquest impersonating
  egress (loopback proxy for Vendors with `egress_mode = Impersonate{..}`).
  Only crate linking `boring-sys`; isolates TLS-fingerprint complexity from
  the rest of the workspace. See
  `docs/superpowers/specs/2026-05-28-egress-impersonation-design.md`.
```

Add to "build pitfalls":

```markdown
- `gw-egress-impersonate` pulls `boring-sys` (via `rquest`) — requires
  `cmake / perl / pkg-config / libclang-dev / build-essential` system deps.
  Adds ~150 MB to the base image and ~2 min cold compile.
```

- [ ] **Step 2: Update `docs/architecture.md`**

In the "Upstream connection" section, add a paragraph explaining the two paths (rustls default + boring loopback egress for Impersonate Vendors). Link to the spec.

- [ ] **Step 3: Update `docs/workflow.md`**

In the `upstream_peer` / `upstream_request_filter` discussion, note the Impersonate branch (HttpPeer points to loopback, headers get X-Egress-* + forged identity).

- [ ] **Step 4: Create `docs/issues/egress-impersonate-known-gaps.md`**

Copy spec §11 (the open-questions table) verbatim as the seed. This is the running operational doc — operators add real observations here once Stage 1/2 rollouts begin.

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md docs/architecture.md docs/workflow.md docs/issues/egress-impersonate-known-gaps.md
git commit -m "docs: ailinkmax-gateway egress impersonation (CLAUDE.md, architecture, workflow, gaps)"
```

---

## Phase 6 — E2E, Performance, Observability

### Task 6.1: E2E test — Impersonate path through fake-upstream

**Files:**
- Create: `crates/gw-proxy/tests/e2e_impersonate.rs`

- [ ] **Step 1: Implement**

```rust
//! Full-stack E2E: gateway picks an Impersonate Vendor → loopback egress →
//! fake-upstream. Asserts the forged identity headers reach the upstream.

use e2e_fake_upstream::start_recorder;

#[tokio::test]
async fn impersonate_claude_code_reaches_upstream_with_stainless_headers() {
    let recorder = start_recorder().await;

    // Seed a Vendor with egress_mode = Impersonate{ClaudeCodeChrome131}
    // and a VendorKey, pointing base_urls at the recorder.
    let test_gw = test_gateway::build()
        .with_vendor("anthropic", &recorder.addr, /*egress_mode*/ "claude_code_chrome131")
        .with_key("anthropic", "vk-test-A")
        .start().await;

    test_gw.send_canonical_anthropic_messages_request().await;

    let req = recorder.last_request().expect("request recorded");
    let h = &req.headers;
    assert!(h.get("user-agent").unwrap().starts_with("Mozilla/5.0"));
    assert!(h.contains_key("x-stainless-package-version"));
    assert!(h.contains_key("anthropic-beta"));
    assert!(h.contains_key("anthropic-version"));
    assert!(!h.contains_key("x-egress-target"));
    assert!(!h.contains_key("x-egress-profile"));
}

#[tokio::test]
async fn per_key_identity_is_stable_across_requests() {
    let recorder = start_recorder().await;
    let test_gw = test_gateway::build()
        .with_vendor("anthropic", &recorder.addr, "claude_code_chrome131")
        .with_key("anthropic", "vk-stable")
        .start().await;

    test_gw.send_canonical_anthropic_messages_request().await;
    let ua_a = recorder.last_request().unwrap().headers.get("user-agent").unwrap().clone();

    test_gw.send_canonical_anthropic_messages_request().await;
    let ua_b = recorder.last_request().unwrap().headers.get("user-agent").unwrap().clone();

    assert_eq!(ua_a, ua_b, "per-key identity must be stable");
}
```

(`test_gateway` is a helper module the engineer must scaffold — wraps Proxy + minimal stores + ledger for tests. If something similar exists already in `crates/gw-gateway/tests/`, reuse it.)

- [ ] **Step 2: Run, verify pass**

```bash
cargo test -p gw-proxy --test e2e_impersonate
```

- [ ] **Step 3: Commit**

```bash
git add crates/gw-proxy/tests/e2e_impersonate.rs
git commit -m "test(gw-proxy): E2E impersonation path through fake-upstream"
```

---

### Task 6.2: Performance microbench — loopback RTT SLO

**Files:**
- Create: `crates/gw-egress-impersonate/benches/loopback_rtt.rs`
- Modify: `crates/gw-egress-impersonate/Cargo.toml` (`[[bench]]` + `criterion` dev-dep)

- [ ] **Step 1: Add criterion dev-dep + bench config**

In `crates/gw-egress-impersonate/Cargo.toml`:

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["async_tokio"] }
tempfile = "3"

[[bench]]
name = "loopback_rtt"
harness = false
```

- [ ] **Step 2: Implement bench**

```rust
//! Measures loopback RTT through the egress (gateway → egress → mock upstream
//! → egress → gateway). Asserts p99 < 200µs (UDS) / 500µs (TCP).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use gw_core::impersonate::ImpersonateProfile;
use gw_egress_impersonate::handle::{EgressBind, EgressHandle};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;

static MOCK_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

fn ensure_mock_upstream(rt: &tokio::runtime::Runtime) -> u16 {
    if let Some(p) = MOCK_PORT.get() { return *p; }
    let port = rt.block_on(async {
        // Reuse the mock_upstream helper from crates/gw-egress-impersonate/tests/common.
        // For the bench, inline a 5-line 200-OK server:
        use hyper::server::conn::http1;
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (s, _) = listener.accept().await.unwrap();
                let io = TokioIo::new(s);
                tokio::spawn(async move {
                    let svc = service_fn(|_req| async {
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::new(http_body_util::Empty::<Bytes>::new()),
                        )
                    });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        });
        port
    });
    MOCK_PORT.set(port).ok();
    port
}

async fn start_egress(transport: &str) -> EgressHandle {
    let bind = match transport {
        "uds" => {
            let dir = tempfile::tempdir().unwrap();
            let p: PathBuf = dir.path().join("egress.sock");
            // Leak the tempdir so the bench has a stable path for the run duration.
            Box::leak(Box::new(dir));
            EgressBind::Uds(p)
        }
        "tcp" => EgressBind::Tcp("127.0.0.1:0".parse().unwrap()),
        _ => unreachable!(),
    };
    EgressHandle::start(bind, /*max_sessions*/ 16).await
}

fn bench_loopback(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all().build().unwrap();
    let mock_port = ensure_mock_upstream(&rt);

    let mut group = c.benchmark_group("loopback_rtt");
    group.measurement_time(Duration::from_secs(10));

    for transport in ["uds", "tcp"] {
        let egress = rt.block_on(start_egress(transport));
        // Build a hyper client that connects to the egress bind point each call.
        // For brevity, this bench does one connect per iter; SLO budget accounts
        // for keep-alive miss in the worst case (re-run with --bench keep-alive
        // for the hot path).
        group.bench_with_input(BenchmarkId::from_parameter(transport), &(), |b, _| {
            b.to_async(&rt).iter(|| async {
                let req = hyper::Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("X-Egress-Target", format!("127.0.0.1:{}", mock_port))
                    .header("X-Egress-Profile", "claude_code_chrome131")
                    .body(Full::new(Bytes::from_static(b"")))
                    .unwrap();
                send_once(&egress, req).await;
            });
        });
    }
    group.finish();
}

async fn send_once(egress: &EgressHandle, req: hyper::Request<Full<Bytes>>) {
    match egress.bind_addr() {
        EgressBind::Tcp(addr) => {
            let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            run_h1(stream, req).await;
        }
        EgressBind::Uds(path) => {
            let stream = tokio::net::UnixStream::connect(path).await.unwrap();
            run_h1(stream, req).await;
        }
    }
}

async fn run_h1<S>(stream: S, req: hyper::Request<Full<Bytes>>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let io = hyper_util::rt::TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(async move { let _ = conn.await; });
    let resp = sender.send_request(req).await.unwrap();
    let _ = resp.into_body().collect().await.unwrap();
}

criterion_group!(benches, bench_loopback);
criterion_main!(benches);
```

- [ ] **Step 3: Run + capture p99**

```bash
cargo bench -p gw-egress-impersonate --bench loopback_rtt
# Inspect target/criterion/loopback_rtt/<param>/report/index.html for p99.
```

Manually verify p99 ≤ 200µs (UDS) and ≤ 500µs (TCP). If exceeded, file an issue per spec §11 #6.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-egress-impersonate/Cargo.toml \
        crates/gw-egress-impersonate/benches/loopback_rtt.rs
git commit -m "bench(gw-egress-impersonate): loopback RTT microbench (UDS + TCP)"
```

---

### Task 6.3: Fingerprint verification (feature-gated)

**Files:**
- Create: `crates/gw-egress-impersonate/tests/fingerprint_verify.rs`
- Modify: `crates/gw-egress-impersonate/Cargo.toml` (`[features] fingerprint-verify`)

- [ ] **Step 1: Add feature**

```toml
[features]
default = []
fingerprint-verify = []
```

- [ ] **Step 2: Implement**

```rust
//! Confirms rquest actually produces a Chrome 131-class JA3/JA4 fingerprint
//! per profile. Gated behind `fingerprint-verify` (CI nightly).
//!
//! Uses a self-hosted JA3/JA4 echo server (e.g. mitm-fingerprint container).
//! Set GW_FINGERPRINT_ECHO_URL=https://your-echo-host to enable.

#![cfg(feature = "fingerprint-verify")]

#[tokio::test]
async fn claude_code_emits_chrome131_ja4() {
    let echo = std::env::var("GW_FINGERPRINT_ECHO_URL")
        .expect("set GW_FINGERPRINT_ECHO_URL to run fingerprint verification");

    // Drive a request through gw-egress-impersonate's pool with
    // ClaudeCodeChrome131; assert the echo response's reported JA4 matches a
    // known-good Chrome 131 hash.
    let pool = gw_egress_impersonate::pool::ClientPool::new(2);
    let client = pool.get_or_create(gw_egress_impersonate::pool::PoolKey {
        profile: gw_core::impersonate::ImpersonateProfile::ClaudeCodeChrome131,
        proxy: None,
    }).await.unwrap();

    let body: serde_json::Value = client.get(&echo).send().await.unwrap()
        .json().await.unwrap();
    let ja4 = body["ja4"].as_str().expect("ja4 field");

    // Snapshot or compare against a known-good Chrome 131 JA4. Update on
    // emulation upgrades.
    insta::assert_snapshot!("ja4_chrome131", ja4);
}
```

- [ ] **Step 3: Add nightly CI step (optional)**

In `.github/workflows/nightly-fingerprint.yml`:

```yaml
name: nightly-fingerprint
on:
  schedule: [{cron: "0 4 * * *"}]
  workflow_dispatch:
jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: |
          cargo test -p gw-egress-impersonate --features fingerprint-verify --test fingerprint_verify
        env:
          GW_FINGERPRINT_ECHO_URL: ${{ secrets.GW_FINGERPRINT_ECHO_URL }}
```

- [ ] **Step 4: Commit**

```bash
git add crates/gw-egress-impersonate/tests/fingerprint_verify.rs \
        crates/gw-egress-impersonate/Cargo.toml \
        .github/workflows/nightly-fingerprint.yml
git commit -m "test(gw-egress-impersonate): fingerprint-verify (feature-gated, nightly CI)"
```

---

### Task 6.4: Real-client header capture scripts + drift CI

**Files:**
- Create: `scripts/capture_real_client_headers/{claude-code,codex,gemini-cli}/`
- Create: `.github/workflows/header-drift.yml`

- [ ] **Step 1: Capture scripts**

Each captures real CLI traffic via a mitm proxy, diffs against the static-header subset in our profile modules. Skeleton in shell:

```bash
#!/bin/bash
# scripts/capture_real_client_headers/claude-code/capture.sh
set -euo pipefail
# 1. spin up mitmproxy on :8080 logging to claude-code.headers.json
# 2. run claude-code CLI through it: HTTPS_PROXY=http://localhost:8080 claude --version-check
# 3. shutdown mitmproxy
# 4. extract just the request headers for /v1/messages
# 5. diff against expected:
EXPECTED_FILE="$(dirname "$0")/expected.json"
OBSERVED_FILE="claude-code.headers.json"
if ! diff <(jq -S . "$EXPECTED_FILE") <(jq -S . "$OBSERVED_FILE"); then
  echo "DRIFT in claude-code headers"
  exit 1
fi
```

Maintain `expected.json` files containing the **static** subset of headers (the per-key/dynamic ones are excluded).

- [ ] **Step 2: Drift CI**

`.github/workflows/header-drift.yml`:

```yaml
name: header-drift
on:
  schedule: [{cron: "0 6 * * 1"}]   # weekly Monday 06:00 UTC
  workflow_dispatch:
jobs:
  drift:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install mitmproxy + CLIs
        run: |
          pip install mitmproxy
          # Install claude-code / codex / gemini-cli per their official docs.
      - name: Run capture diffs
        run: |
          ./scripts/capture_real_client_headers/claude-code/capture.sh
          ./scripts/capture_real_client_headers/codex/capture.sh
          ./scripts/capture_real_client_headers/gemini-cli/capture.sh
```

- [ ] **Step 3: Commit**

```bash
git add scripts/capture_real_client_headers/ .github/workflows/header-drift.yml
git commit -m "ci(headers): weekly drift detection against real Claude Code/Codex/Gemini CLI"
```

---

### Task 6.5: Metrics summary

The metrics keys are emitted from the relevant code in prior phases (Task 2.7 `gw_egress_lazy_persist_total`, Task 3.3 `gw_egress_client_pool_total`, Task 3.7 `gw_egress_supervisor_restart_total`, Task 5.2 `gw_egress_disabled_total`).

- [ ] **Step 1: Verify all spec §8.6 metrics are emitted somewhere**

```bash
grep -rnE "gw_egress_" crates/ | sort -u
```

Expected: every key from spec §8.6 (`requests_total`, `error_total`, `client_pool_total`, `handshake_seconds`, `loopback_rtt_seconds`, `lazy_persist_total`, `identity_cache_total`, `disabled_total`, `supervisor_restart_total`) has at least one emit site.

Missing ones — add them. For each missing metric:
- `gw_egress_requests_total` / `gw_egress_error_total` — emit in `server::Handler::handle` after final response.
- `gw_egress_handshake_seconds` — wrap rquest first-use timing in `pool::build_client`.
- `gw_egress_loopback_rtt_seconds` — emit in gateway-side `upstream_response_filter` when egress path (use `ctx.egress_profile.is_some()`).
- `gw_egress_identity_cache_total{kind="turn_state"}` — emit in `TurnStateCache::get_or_create` (hit vs miss).

- [ ] **Step 2: Build + run**

```bash
cargo build --workspace
cargo test --workspace --no-fail-fast
```

- [ ] **Step 3: Commit**

```bash
git add -u
git commit -m "feat(gw-egress-impersonate): emit all spec §8.6 observability metrics"
```

---

### Task 6.6: Final regression + handoff

- [ ] **Step 1: Run full suite + bench**

```bash
cargo test --workspace --no-fail-fast
cargo test -p gw-proxy --test regression_rustls_baseline
cargo bench -p gw-egress-impersonate --bench loopback_rtt
```

Expected: all tests pass; bench p99 within SLO.

- [ ] **Step 2: Update `docs/issues/egress-impersonate-known-gaps.md` with any findings discovered during implementation**

- [ ] **Step 3: Final commit**

```bash
git add docs/issues/egress-impersonate-known-gaps.md
git commit -m "docs(issues): seed egress-impersonate gaps with implementation findings"
```

- [ ] **Step 4: Tag the work**

```bash
git tag -a egress-impersonation-impl-ready -m "Egress impersonation: all phases complete, ready for Stage 1 rollout"
```

Proceed to spec §9.3 Stage 1 (single test vendor) for live rollout.

---

## Verification Summary

After all phases complete:

- [ ] `cargo build --workspace` clean
- [ ] `cargo test --workspace` green
- [ ] `cargo test -p gw-proxy --test regression_rustls_baseline` green (default path unchanged)
- [ ] `cargo test -p gw-proxy --test e2e_impersonate` green (impersonation path works through fake-upstream)
- [ ] `cargo bench -p gw-egress-impersonate --bench loopback_rtt` shows p99 < 200µs (UDS) / < 500µs (TCP)
- [ ] `docker build .` succeeds with boring-sys deps
- [ ] Migration 0009 applies cleanly + idempotent on re-run
- [ ] Spec §11 open questions tracked in `docs/issues/egress-impersonate-known-gaps.md`
- [ ] All spec §8.6 metrics have at least one emit site
- [ ] Phase 0 baseline snapshot file unchanged from initial capture

Stage 1 rollout: pick one non-critical test Vendor, set `egress_mode = Impersonate{ClaudeCodeChrome131}` via admin, send 100 real requests, monitor `gw_egress_requests_total{result=success}` ≥ 99% and absence of `gw_egress_error_total{class=tls|h2_protocol}` clustering.
