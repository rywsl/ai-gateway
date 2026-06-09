# W6 Rust OAuth Refresh Write-Back Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Rust the sole runtime OAuth refresh writer for vendor keys while Go remains the configuration and initial authorization writer.

**Architecture:** W6 hangs the runtime refresher/cache behind the W5 `AdapterCtx::oauth_runtime` slot and keeps auth injection inside the outbound adapter seam. The refresher reloads the current `Versioned<VendorKey>` from `EntityStore`, performs Redis single-flight across pods, refreshes with the provider token endpoint, persists encrypted token updates with version CAS, and broadcasts `events:entity ns=vendor_key op=upsert` so every pod reloads the new token. Go contracts are updated in the same wave so there is no Go runtime refresh double-writer after K-C.

**Tech Stack:** Rust 1.88 workspace, `gw-adapter`, `gw-store`, `gw-proxy`, `gw-gateway`, `gw-core::entity::OAuthCredentials`, PgSQL JSONB `vendor_keys.payload.oauth`, Redis `events:entity`, Redis `SET NX PX` single-flight, AES-256-GCM `KekCache`, `reqwest`, `wiremock`, `testcontainers`.

---

## Scope Boundaries

- Do not touch the blocked W2 source worktree.
- Do not let Go refresh `vendor_keys.payload.oauth` runtime tokens after K-C.
- Do not change Go ownership of OAuth provider configuration, OAuth code exchange, or first-time authorization seed writes.
- Do not add a second refresh writer in Go for retry fallback; Rust lost-lock or lost-CAS paths reload the winner's token.
- Do not send expired OAuth access tokens after the W6 adapter hook lands; refresh before bearer injection or fail the request with an adapter auth error.
- Do not change the `vendor_keys.payload.oauth` schema unless implementation evidence shows JSONB update cannot preserve existing fields. Current evidence says a schema change is not required.
- Do not remove the `oauth_failed` reverse-XADD without the user decision in this plan being resolved. The recommended path is status-only repurpose.

## Current Evidence

- F2 authority is confirmed in `docs/design/2026-06-09-roadmap-authority-and-scope.md`: Go remains config writer and Rust becomes the sole runtime refresh writer.
- W0 evidence in `docs/plans/2026-06-09-w0-contract-evidence.md` requires encrypted access-token write-back, rotated refresh-token preservation, `version + 1`, CAS, and `events:entity ns=vendor_key op=upsert`.
- W5 seam exists in `crates/gw-adapter/src/ctx.rs` as `AdapterCtx::oauth_runtime: Option<&Arc<dyn OAuthRuntime>>`, but the current `OAuthRuntime::get_access_token` only returns `Option<SecretString>` and has no refresh/write-back result contract.
- `crates/gw-store/src/loader.rs` decrypts `payload.oauth.access_token_enc` and `refresh_token_enc` into in-process `OAuthCredentials`, including `token_endpoint`, `scope`, and `quota_project_id`.
- `crates/gw-store/src/encrypt.rs` has production `decrypt` and only `encrypt_for_test_only`; W6 must add a production encrypt helper before Rust write-back can be correct.
- `crates/gw-store/src/quota_state.rs::QuotaStateWriter::write` already shows the local pattern for `version = version + 1`, Redis L1 `DEL entity:vendor_key:<id>`, and `XADD events:entity`, but it is not CAS-returning and is not sufficient for OAuth write-back by itself.
- `crates/gw-proxy/src/filters/upstream.rs::apply_outbound_adapter` currently passes `oauth_runtime: None`, so W6 must wire a real runtime handle from gateway/proxy bootstrap.
- `crates/gw-proxy/src/error/listeners/oauth.rs` currently reverse-XADDs `field=oauth_failed` as a Go refresh command; this conflicts with post-K-C ownership unless deleted or repurposed as status-only.

## File Structure

- Create `crates/gw-store/src/oauth_refresh.rs`
  - Own OAuth write-back input/output structs, SQL builder, Redis lock helper, and `PgOAuthRefreshWriter`.
  - Persist encrypted access token and rotated refresh token when present.
  - Preserve the old encrypted refresh token when the provider omits a rotated refresh token.
  - Require `WHERE id = $1 AND version = $expected_version AND deleted_at IS NULL`.
  - Return the new version from `UPDATE ... RETURNING version`.
  - Delete `entity:vendor_key:<id>` and XADD `events:entity * ns vendor_key id <id> op upsert v <new_version> ts_ms <now> source rust_oauth_refresh`.
- Modify `crates/gw-store/src/encrypt.rs`
  - Add production `encrypt_with_active_kek(plain: &SecretString, cache: &KekCache) -> anyhow::Result<EncryptedKey>`.
  - Keep `encrypt_for_test_only` for deterministic test construction only.
- Modify `crates/gw-store/src/lib.rs`
  - Export the new `oauth_refresh` module and writer types.
- Modify `crates/gw-store/src/entity_store.rs`
  - Expose loader-backed `PgPool` and `KekCache` handles if the runtime builder needs them, or pass the existing `LoaderCtx` into the runtime at gateway bootstrap.
  - Do not expose mutable cache internals.
- Modify `crates/gw-adapter/src/ctx.rs`
  - Replace the current `OAuthRuntime::get_access_token` return shape with a refresh-aware method that returns token plus refreshed version metadata.
- Modify `crates/gw-adapter/src/noop.rs`
  - Before bearer injection, call `ctx.oauth_runtime` when `vendor_key.oauth.is_some()`.
  - If no runtime handle exists for an OAuth key, return an adapter error instead of sending an expired token after W6.
  - Preserve static API-key behavior for non-OAuth keys.
- Modify `crates/gw-proxy/src/filters/upstream.rs`
  - Pass the real OAuth runtime handle into `AdapterCtx`.
  - After a successful refresh, replace `ctx.picked_vendor_key` with the refreshed `Arc<VendorKey>` returned by the runtime so downstream egress identity headers see the current OAuth quota project.
- Modify `crates/gw-proxy/src/ctx.rs` and `crates/gw-proxy/src/gateway.rs`
  - Carry `Arc<dyn gw_adapter::OAuthRuntime>` in gateway state and request rewrite calls.
- Modify `crates/gw-gateway/src/lib.rs`
  - Build the runtime from `EntityStore`, `PgOAuthRefreshWriter`, Redis lock config, and `reqwest::Client`.
- Modify `crates/gw-gateway/src/config.rs`
  - Add `[oauth_refresh]` settings: `lock_ttl_ms`, `refresh_skew_secs`, `request_timeout_ms`, `wait_after_lock_lost_ms`.
- Modify `crates/gw-gateway/src/metrics_catalog.rs`
  - Register post-K-C metrics and update or retire `gw_oauth_refresh_failed_xadd_total` depending on the user decision.
- Modify `crates/gw-proxy/src/error/listeners/oauth.rs`
  - Repurpose or delete the old reverse-XADD branch based on the user decision.
- Modify docs:
  - `docs/go-contracts/contract-change/2026-06-09-rust-oauth-refresh.md`
  - `docs/go-contracts/parts/02-go-as-consumer.md`
  - `docs/go-contracts/parts/03-shared-schema.md`
  - `docs/go-contracts/parts/07-oauth-ldap.md`
  - `docs/go-contracts/parts/08-control-plane.md`
  - `docs/spec/07-vendor-integration.md`
  - `docs/ROADMAP.md`

## User Decision Needed

### Decision: Retire Or Repurpose Old `oauth_failed` Reverse-XADD

Post-K-C, `events:entity field=oauth_failed` cannot remain a Go refresh command because F2 says Rust is the sole runtime refresh writer.

**Option 1: Repurpose to status-only event. Recommended.**

Implementation difference:

- Keep `OAuthListener` as the response-side signal listener.
- Change the emitted event from:

```text
XADD events:entity * ns vendor_key id <key_id> op status_change field oauth_failed reason upstream_401 ts_ms <now>
```

to:

```text
XADD events:entity * ns vendor_key id <key_id> op status_change field oauth_refresh_status status rust_refresh_failed reason upstream_401 ts_ms <now> source rust_oauth_refresh
```

- Rename the metric from `gw_oauth_refresh_failed_xadd_total` to `gw_oauth_refresh_status_xadd_total`.
- Update Go contracts to state that this event is an alert/status signal only and must not trigger Go refresh.
- Keep affinity invalidation on `RefreshFailed`.

Why recommended: it preserves operator visibility for upstream 401 or refresh failures while removing the command semantics that would create a double-writer.

**Option 2: Delete the reverse-XADD branch entirely.**

Implementation difference:

- Remove the `events:entity` XADD from `OAuthListener`.
- Keep only local metrics and affinity invalidation.
- Delete `gw_oauth_refresh_failed_xadd_total` from `metrics_catalog.rs`.
- Update source tripwire tests to assert the old command event is absent.

Tradeoff: this is simpler, but Go loses a stream-visible signal for OAuth refresh failures unless it reads Prometheus or gateway runtime status.

**Plan action:** implement Option 1 unless the user explicitly chooses Option 2 before W6 source execution starts.

### Decision: Schema Change

No user decision is needed for schema based on current evidence. `vendor_keys.payload.oauth` already contains `access_token_enc`, `refresh_token_enc`, `access_expires_at`, `token_endpoint`, `scope`, and `quota_project_id`; W6 can update the JSONB sub-object with `jsonb_set` while preserving fields. If source execution proves that a dedicated column is required for CAS or encrypted token payloads, stop and return this evidence packet before changing migrations:

```text
User Decision Needed: OAuth write-back schema change
Observed SQL/API failure:
Current payload.oauth shape:
Why jsonb_set cannot preserve fields:
Options:
1. Keep JSONB-only write-back and adjust writer implementation.
2. Add structured oauth runtime columns and migration.
Recommended option:
```

## Task W6.1: Contract Docs Commit

**Files:**
- Create: `docs/go-contracts/contract-change/2026-06-09-rust-oauth-refresh.md`
- Modify: `docs/go-contracts/parts/02-go-as-consumer.md`
- Modify: `docs/go-contracts/parts/03-shared-schema.md`
- Modify: `docs/go-contracts/parts/07-oauth-ldap.md`
- Modify: `docs/go-contracts/parts/08-control-plane.md`

- [ ] **Step 1: Add the contract-change document**

Create `docs/go-contracts/contract-change/2026-06-09-rust-oauth-refresh.md` with these sections and exact ownership statements:

````markdown
# Contract Change: Rust Runtime OAuth Refresh

- Date: 2026-06-09
- Status: Planned for W6 K-C
- Authority: F2 in `docs/design/2026-06-09-roadmap-authority-and-scope.md`

## Ownership

Go remains the writer for OAuth provider configuration and initial vendor-key authorization seed data.
Rust is the sole runtime writer for refreshed `vendor_keys.payload.oauth` after K-C.
Go must not refresh or overwrite runtime vendor-key OAuth tokens after K-C.

## Rust Runtime Write-Back

Rust writes `vendor_keys.payload.oauth` with:

- `access_token_enc`: encrypted refreshed access token.
- `refresh_token_enc`: encrypted rotated refresh token when the provider returns one; the existing encrypted refresh token is preserved when the provider omits one.
- `access_expires_at`: refreshed expiry timestamp.
- `token_endpoint`: preserved from the previous payload.
- `scope`: refreshed scope if returned by provider, otherwise preserved from the previous payload.
- `quota_project_id`: preserved from the previous payload.

The write is optimistic-CAS:

```sql
UPDATE vendor_keys
SET payload = jsonb_set(payload, '{oauth}', $oauth_json::jsonb, true),
    version = version + 1,
    updated_at = NOW()
WHERE id = $key_id
  AND version = $expected_version
  AND deleted_at IS NULL
RETURNING version
```

After commit, Rust publishes:

```text
XADD events:entity * ns vendor_key id <key_id> op upsert v <new_version> ts_ms <now> source rust_oauth_refresh
```

## Single-Flight

Rust uses `SET lock:oauth:refresh:<key_id> <owner> NX PX <ttl_ms>` before contacting the token endpoint.
Lock losers reload `vendor_key` and use the winner's token.
CAS losers reload `vendor_key` and use the winner's token.

## `oauth_failed`

Post-K-C, Go must not consume `field=oauth_failed` or any OAuth status event as a refresh command.
If Rust emits an OAuth failure event, it is status-only.
````

Run:

```bash
rg -n "sole runtime writer|Go must not refresh|SET lock:oauth:refresh|source rust_oauth_refresh" docs/go-contracts/contract-change/2026-06-09-rust-oauth-refresh.md
```

Expected: PASS with all four phrases present.

- [ ] **Step 2: Update Go consumer contract**

In `docs/go-contracts/parts/02-go-as-consumer.md`, change the `vendor_key.oauth_failed` subsection so it says the old refresh command is legacy/pre-K-C only. Add this exact post-K-C rule:

```markdown
Post-K-C rule: Go must not refresh `vendor_keys.payload.oauth` in response to Rust OAuth runtime events. Rust-owned OAuth events are status/alert signals only; runtime refresh and encrypted write-back are owned by Rust.
```

Run:

```bash
rg -n "Post-K-C rule: Go must not refresh|legacy/pre-K-C" docs/go-contracts/parts/02-go-as-consumer.md
```

Expected: PASS with both phrases present.

- [ ] **Step 3: Update shared schema contract**

In `docs/go-contracts/parts/03-shared-schema.md` section `3.2.5.1`, replace the Go-only runtime refresh wording with split ownership:

```markdown
**Writer responsibility after K-C**:

- Go writes OAuth configuration and initial authorization seed data.
- Rust writes runtime refresh results for `vendor_keys.payload.oauth`.
- Rust reads `token_endpoint` for vendor-key runtime refresh.
- Rust encrypts refreshed tokens with the active KEK and persists with version CAS.
- Rust emits `events:entity ns=vendor_key op=upsert` after a successful write.
- Go must not perform runtime refresh write-back for this sub-object after K-C.
```

Run:

```bash
rg -n "Rust reads `token_endpoint`|version CAS|Go must not perform runtime refresh" docs/go-contracts/parts/03-shared-schema.md
```

Expected: PASS with all phrases present.

- [ ] **Step 4: Separate auth-cache OAuth from vendor-key OAuth**

In `docs/go-contracts/parts/07-oauth-ldap.md`, keep Go-owned user authentication provider/cache language, then add this note near `7.3`:

```markdown
This section governs user authentication provider configuration and `auth:oauth:token:*` caches. It does not govern vendor-key runtime OAuth credentials in `vendor_keys.payload.oauth`. After K-C, vendor-key runtime refresh is Rust-owned per `contract-change/2026-06-09-rust-oauth-refresh.md`.
```

Run:

```bash
rg -n "auth:oauth:token|vendor-key runtime refresh is Rust-owned" docs/go-contracts/parts/07-oauth-ldap.md
```

Expected: PASS with both phrases present.

- [ ] **Step 5: Update control-plane `refresh-oauth` command**

In `docs/go-contracts/parts/08-control-plane.md`, update the `refresh-oauth` row so the internal API is:

```markdown
F2 K-C Rust `OAuthRuntime` single-flight refresher; Go issues the command, Rust performs refresh and write-back
```

Run:

```bash
rg -n "Rust `OAuthRuntime` single-flight refresher|Rust performs refresh and write-back" docs/go-contracts/parts/08-control-plane.md
```

Expected: PASS with both phrases present.

- [ ] **Step 6: Verify contract docs and commit**

Run:

```bash
rg -n "Go.*runtime refresh|Rust.*runtime refresh|oauth_failed|token_endpoint" docs/go-contracts/parts docs/go-contracts/contract-change/2026-06-09-rust-oauth-refresh.md
git diff --check
```

Expected: PASS. Remaining `Go.*runtime refresh` matches must say Go must not runtime refresh after K-C or must be clearly marked legacy/pre-K-C.

Commit:

```bash
git add docs/go-contracts/contract-change/2026-06-09-rust-oauth-refresh.md docs/go-contracts/parts/02-go-as-consumer.md docs/go-contracts/parts/03-shared-schema.md docs/go-contracts/parts/07-oauth-ldap.md docs/go-contracts/parts/08-control-plane.md
git commit -m "docs(oauth): move runtime refresh ownership to rust"
```

## Task W6.2: Store Writer Commit

**Files:**
- Modify: `crates/gw-store/src/encrypt.rs`
- Create: `crates/gw-store/src/oauth_refresh.rs`
- Modify: `crates/gw-store/src/lib.rs`
- Test: `crates/gw-store/src/encrypt.rs`
- Test: `crates/gw-store/src/oauth_refresh.rs`

- [ ] **Step 1: Add failing production encrypt test**

In `crates/gw-store/src/encrypt.rs`, add this test under the existing test module or create a `#[cfg(test)] mod production_encrypt_tests`:

```rust
#[test]
fn encrypt_with_active_kek_roundtrips_secret() {
    let kek = [7u8; 32];
    let cache = KekCache::with_active(
        std::collections::HashMap::from([("active".to_string(), kek)]),
        Some("active"),
    );
    let secret = secrecy::SecretString::from("access-token-1");
    let encrypted = encrypt_with_active_kek(&secret, &cache).unwrap();
    assert_eq!(encrypted.kek_kid, "active");
    let decrypted = decrypt(&encrypted, &cache).unwrap();
    assert_eq!(secrecy::ExposeSecret::expose_secret(&decrypted), "access-token-1");
}
```

Run:

```bash
cargo test -p gw-store encrypt_with_active_kek_roundtrips_secret
```

Expected: FAIL with `cannot find function encrypt_with_active_kek`.

- [ ] **Step 2: Implement production encryption helper**

In `crates/gw-store/src/encrypt.rs`, add:

```rust
pub fn encrypt_with_active_kek(plain: &SecretString, cache: &KekCache) -> anyhow::Result<EncryptedKey> {
    use aes_gcm::aead::{AeadCore, OsRng};
    use secrecy::ExposeSecret;

    let kid = cache.active_kid();
    let kek = cache
        .get(&kid)
        .ok_or_else(|| anyhow::anyhow!("active kek not found: {kid}"))?;
    let cipher = Aes256Gcm::new(kek.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plain.expose_secret().as_bytes())
        .map_err(|_| anyhow::anyhow!("encrypt failed"))?;
    let split_at = ct
        .len()
        .checked_sub(16)
        .ok_or_else(|| anyhow::anyhow!("ciphertext too short"))?;
    let (ciphertext, auth_tag) = ct.split_at(split_at);
    Ok(EncryptedKey {
        ciphertext: ciphertext.to_vec(),
        nonce: nonce.to_vec(),
        auth_tag: auth_tag.to_vec(),
        kek_kid: kid,
    })
}
```

Run:

```bash
cargo test -p gw-store encrypt_with_active_kek_roundtrips_secret
```

Expected: PASS.

- [ ] **Step 3: Add failing OAuth write-back SQL tests**

Create `crates/gw-store/src/oauth_refresh.rs` with only test scaffolding first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_sql_uses_version_cas_and_returns_new_version() {
        let sql = update_sql();
        assert!(sql.contains("jsonb_set(payload, '{oauth}'"));
        assert!(sql.contains("version = version + 1"));
        assert!(sql.contains("WHERE id = $1 AND version = $2 AND deleted_at IS NULL"));
        assert!(sql.contains("RETURNING version"));
    }

    #[test]
    fn entity_event_fields_are_vendor_key_upsert() {
        let fields = entity_event_fields("vk-1", 12, 1770000000000);
        assert!(fields.contains(&("ns".to_string(), "vendor_key".to_string())));
        assert!(fields.contains(&("id".to_string(), "vk-1".to_string())));
        assert!(fields.contains(&("op".to_string(), "upsert".to_string())));
        assert!(fields.contains(&("v".to_string(), "12".to_string())));
        assert!(fields.contains(&("source".to_string(), "rust_oauth_refresh".to_string())));
    }
}
```

Add `pub mod oauth_refresh;` to `crates/gw-store/src/lib.rs`.

Run:

```bash
cargo test -p gw-store oauth_refresh
```

Expected: FAIL with missing `update_sql` and `entity_event_fields`.

- [ ] **Step 4: Implement write-back data types, SQL, and event fields**

In `crates/gw-store/src/oauth_refresh.rs`, add:

```rust
use crate::encrypt::{encrypt_with_active_kek, EncryptedKey, KekCache};
use crate::redis_client::RedisCtx;
use fred::interfaces::{KeysInterface, StreamsInterface};
use secrecy::SecretString;
use sqlx::PgPool;
use time::OffsetDateTime;

#[derive(Clone, Debug)]
pub struct OAuthWriteBack {
    pub key_id: String,
    pub expected_version: u64,
    pub access_token: SecretString,
    pub rotated_refresh_token: Option<SecretString>,
    pub previous_refresh_token_enc: EncryptedKey,
    pub access_expires_at: OffsetDateTime,
    pub token_endpoint: String,
    pub scope: Vec<String>,
    pub quota_project_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthWriteBackResult {
    pub key_id: String,
    pub new_version: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthWriteBackError {
    #[error("oauth write CAS lost for key {key_id} at expected version {expected_version}")]
    CasLost { key_id: String, expected_version: u64 },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub fn update_sql() -> &'static str {
    "UPDATE vendor_keys \
     SET payload = jsonb_set(payload, '{oauth}', $3::jsonb, true), \
         version = version + 1, \
         updated_at = NOW() \
     WHERE id = $1 AND version = $2 AND deleted_at IS NULL \
     RETURNING version"
}

pub fn entity_event_fields(key_id: &str, new_version: u64, ts_ms: i64) -> Vec<(String, String)> {
    vec![
        ("ns".to_string(), "vendor_key".to_string()),
        ("id".to_string(), key_id.to_string()),
        ("op".to_string(), "upsert".to_string()),
        ("v".to_string(), new_version.to_string()),
        ("ts_ms".to_string(), ts_ms.to_string()),
        ("source".to_string(), "rust_oauth_refresh".to_string()),
    ]
}
```

Run:

```bash
cargo test -p gw-store oauth_refresh::tests::update_sql_uses_version_cas_and_returns_new_version oauth_refresh::tests::entity_event_fields_are_vendor_key_upsert
```

Expected: PASS.

- [ ] **Step 5: Add refresh-token preservation test**

Add this test to `oauth_refresh.rs`:

```rust
#[test]
fn oauth_payload_preserves_refresh_token_when_provider_omits_rotation() {
    let previous = EncryptedKey {
        ciphertext: vec![1],
        nonce: vec![2],
        auth_tag: vec![3],
        kek_kid: "old".to_string(),
    };
    let payload = build_oauth_payload(
        EncryptedKey {
            ciphertext: vec![4],
            nonce: vec![5],
            auth_tag: vec![6],
            kek_kid: "new".to_string(),
        },
        previous.clone(),
        OffsetDateTime::from_unix_timestamp(1_770_000_000).unwrap(),
        "https://oauth.example/token".to_string(),
        vec!["scope-a".to_string()],
        Some("quota-project".to_string()),
    );
    assert_eq!(payload["refresh_token_enc"]["kek_kid"], "old");
    assert_eq!(payload["access_token_enc"]["kek_kid"], "new");
    assert_eq!(payload["token_endpoint"], "https://oauth.example/token");
    assert_eq!(payload["quota_project_id"], "quota-project");
}
```

Run:

```bash
cargo test -p gw-store oauth_payload_preserves_refresh_token_when_provider_omits_rotation
```

Expected: FAIL with missing `build_oauth_payload`.

- [ ] **Step 6: Implement payload builder**

Add:

```rust
pub fn build_oauth_payload(
    access_token_enc: EncryptedKey,
    refresh_token_enc: EncryptedKey,
    access_expires_at: OffsetDateTime,
    token_endpoint: String,
    scope: Vec<String>,
    quota_project_id: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "access_token_enc": access_token_enc,
        "refresh_token_enc": refresh_token_enc,
        "access_expires_at": access_expires_at
            .format(&time::format_description::well_known::Rfc3339)
            .expect("RFC3339 formatting for OffsetDateTime must succeed"),
        "token_endpoint": token_endpoint,
        "scope": scope,
        "quota_project_id": quota_project_id,
    })
}
```

Run:

```bash
cargo test -p gw-store oauth_payload_preserves_refresh_token_when_provider_omits_rotation
```

Expected: PASS.

- [ ] **Step 7: Implement `PgOAuthRefreshWriter::write`**

Add:

```rust
#[derive(Clone)]
pub struct PgOAuthRefreshWriter {
    pg: PgPool,
    redis: RedisCtx,
    kek: KekCache,
}

impl PgOAuthRefreshWriter {
    pub fn new(pg: PgPool, redis: RedisCtx, kek: KekCache) -> Self {
        Self { pg, redis, kek }
    }

    pub async fn write(&self, input: OAuthWriteBack) -> Result<OAuthWriteBackResult, OAuthWriteBackError> {
        let access_token_enc = encrypt_with_active_kek(&input.access_token, &self.kek)?;
        let refresh_token_enc = match input.rotated_refresh_token.as_ref() {
            Some(token) => encrypt_with_active_kek(token, &self.kek)?,
            None => input.previous_refresh_token_enc,
        };
        let payload = build_oauth_payload(
            access_token_enc,
            refresh_token_enc,
            input.access_expires_at,
            input.token_endpoint,
            input.scope,
            input.quota_project_id,
        );
        let row: Option<(i64,)> = sqlx::query_as(update_sql())
            .bind(&input.key_id)
            .bind(input.expected_version as i64)
            .bind(payload)
            .fetch_optional(&self.pg)
            .await
            .map_err(anyhow::Error::from)?;
        let Some((new_version_i64,)) = row else {
            return Err(OAuthWriteBackError::CasLost {
                key_id: input.key_id,
                expected_version: input.expected_version,
            });
        };
        let new_version = new_version_i64 as u64;
        let client = self.redis.pool.next();
        let _: Result<i64, _> = client.del(format!("entity:vendor_key:{}", input.key_id)).await;
        let now_ms = now_ms();
        let _: Result<(), _> = client
            .xadd(
                "events:entity",
                false,
                None::<()>,
                fred::types::XID::Auto,
                entity_event_fields(&input.key_id, new_version, now_ms),
            )
            .await;
        Ok(OAuthWriteBackResult {
            key_id: input.key_id,
            new_version,
        })
    }
}

fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}
```

Run:

```bash
cargo test -p gw-store oauth_refresh
```

Expected: PASS.

- [ ] **Step 8: Run store verification and commit**

Run:

```bash
cargo test -p gw-store encrypt oauth_refresh
git diff --check
```

Expected: PASS.

Commit:

```bash
git add crates/gw-store/src/encrypt.rs crates/gw-store/src/oauth_refresh.rs crates/gw-store/src/lib.rs
git commit -m "feat(oauth): persist refreshed tokens and broadcast vendor key"
```

## Task W6.3: Runtime Refresher And Cache Commit

**Files:**
- Modify: `crates/gw-adapter/src/ctx.rs`
- Create: `crates/gw-adapter/src/oauth_runtime.rs`
- Modify: `crates/gw-adapter/src/lib.rs`
- Test: `crates/gw-adapter/src/oauth_runtime.rs`
- Modify: `crates/gw-adapter/Cargo.toml`

- [ ] **Step 1: Add runtime dependencies**

In `crates/gw-adapter/Cargo.toml`, add:

```toml
gw-store = { path = "../gw-store" }
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
fred.workspace = true
```

Run:

```bash
cargo check -p gw-adapter
```

Expected: PASS because no code uses the dependencies yet.

- [ ] **Step 2: Replace the OAuth runtime trait shape**

In `crates/gw-adapter/src/ctx.rs`, replace `get_access_token` with:

```rust
pub struct OAuthTokenResult {
    pub access_token: secrecy::SecretString,
    pub refreshed_vendor_key: Option<Arc<VendorKey>>,
    pub version: u64,
}

#[async_trait]
pub trait OAuthRuntime: Send + Sync {
    async fn access_token_for(
        &self,
        vendor_key_id: &KeyId,
        request_id: &str,
    ) -> Result<OAuthTokenResult, String>;
}
```

Run:

```bash
cargo check -p gw-adapter
```

Expected: FAIL in any tests or adapters that still call `get_access_token`.

- [ ] **Step 3: Add failing lock-loser test**

Create `crates/gw-adapter/src/oauth_runtime.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_refresh_uses_skew_window() {
        let now = time::OffsetDateTime::from_unix_timestamp(1_000).unwrap();
        let expires = time::OffsetDateTime::from_unix_timestamp(1_030).unwrap();
        assert!(should_refresh(now, expires, 60));
        assert!(!should_refresh(now, expires, 10));
    }

    #[test]
    fn lock_key_is_per_vendor_key() {
        assert_eq!(lock_key("vk-1"), "lock:oauth:refresh:vk-1");
    }
}
```

Export it in `crates/gw-adapter/src/lib.rs`:

```rust
pub mod oauth_runtime;
```

Run:

```bash
cargo test -p gw-adapter oauth_runtime
```

Expected: FAIL with missing `should_refresh` and `lock_key`.

- [ ] **Step 4: Implement runtime types and helper functions**

In `oauth_runtime.rs`, add:

```rust
use crate::ctx::{OAuthRuntime, OAuthTokenResult};
use async_trait::async_trait;
use gw_core::ids::KeyId;
use gw_store::oauth_refresh::{OAuthWriteBack, OAuthWriteBackError, PgOAuthRefreshWriter};
use gw_store::EntityStore;
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use time::OffsetDateTime;

#[derive(Clone, Debug)]
pub struct OAuthRuntimeCfg {
    pub refresh_skew_secs: i64,
    pub lock_ttl_ms: u64,
    pub request_timeout_ms: u64,
    pub wait_after_lock_lost_ms: u64,
}

pub fn should_refresh(now: OffsetDateTime, expires_at: OffsetDateTime, skew_secs: i64) -> bool {
    now >= expires_at - time::Duration::seconds(skew_secs)
}

pub fn lock_key(key_id: &str) -> String {
    format!("lock:oauth:refresh:{key_id}")
}
```

Run:

```bash
cargo test -p gw-adapter oauth_runtime::tests::should_refresh_uses_skew_window oauth_runtime::tests::lock_key_is_per_vendor_key
```

Expected: PASS.

- [ ] **Step 5: Add provider response parser test**

Add:

```rust
#[test]
fn token_response_preserves_scope_when_scope_is_absent() {
    let body = br#"{"access_token":"new-access","expires_in":3600,"token_type":"bearer"}"#;
    let parsed = parse_token_response(body, &["old-scope".to_string()]).unwrap();
    assert_eq!(parsed.access_token.expose_secret(), "new-access");
    assert!(parsed.rotated_refresh_token.is_none());
    assert_eq!(parsed.scope, vec!["old-scope".to_string()]);
}
```

Run:

```bash
cargo test -p gw-adapter token_response_preserves_scope_when_scope_is_absent
```

Expected: FAIL with missing `parse_token_response`.

- [ ] **Step 6: Implement token response parsing**

Add:

```rust
pub struct ParsedTokenResponse {
    pub access_token: SecretString,
    pub rotated_refresh_token: Option<SecretString>,
    pub expires_in_secs: i64,
    pub scope: Vec<String>,
}

#[derive(serde::Deserialize)]
struct TokenEndpointResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

pub fn parse_token_response(body: &[u8], previous_scope: &[String]) -> anyhow::Result<ParsedTokenResponse> {
    let decoded: TokenEndpointResponse = serde_json::from_slice(body)?;
    let scope = decoded
        .scope
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_else(|| previous_scope.to_vec());
    Ok(ParsedTokenResponse {
        access_token: SecretString::from(decoded.access_token),
        rotated_refresh_token: decoded.refresh_token.map(SecretString::from),
        expires_in_secs: decoded.expires_in.unwrap_or(3600),
        scope,
    })
}
```

Run:

```bash
cargo test -p gw-adapter token_response_preserves_scope_when_scope_is_absent
```

Expected: PASS.

- [ ] **Step 7: Implement `RustOAuthRuntime` reload/single-flight/CAS behavior**

Add a `RustOAuthRuntime` struct that:

- Holds `store: Arc<EntityStore>`, `writer: Arc<PgOAuthRefreshWriter>`, `redis: gw_store::redis_client::RedisCtx`, `client: reqwest::Client`, and `cfg: OAuthRuntimeCfg`.
- Implements `OAuthRuntime::access_token_for`.
- Loads the current `Versioned<VendorKey>` with `store.vendor_key_or_load(vendor_key_id).await`.
- Returns the current token without provider I/O if `should_refresh(now, oauth.access_expires_at, cfg.refresh_skew_secs)` is false.
- Acquires Redis lock with `SET lock:oauth:refresh:<key_id> <request_id> NX PX <lock_ttl_ms>`.
- If the lock is lost, sleeps `wait_after_lock_lost_ms`, invalidates local vendor key cache, reloads, and returns the winner's token.
- Sends `POST token_endpoint` with `grant_type=refresh_token` and the current refresh token.
- Builds `OAuthWriteBack` using the current `Versioned<VendorKey>.version` as `expected_version`.
- On `OAuthWriteBackError::CasLost`, invalidates local vendor key cache, reloads, and returns the winner's token.
- On successful write, invalidates local vendor key cache, reloads, and returns `OAuthTokenResult` with `refreshed_vendor_key: Some(reloaded.value.clone())`.

Use these metrics labels:

```rust
metrics::counter!("gw_oauth_runtime_refresh_total", "result" => "ok").increment(1);
metrics::counter!("gw_oauth_runtime_refresh_total", "result" => "lock_lost").increment(1);
metrics::counter!("gw_oauth_runtime_refresh_total", "result" => "cas_lost").increment(1);
metrics::counter!("gw_oauth_runtime_refresh_total", "result" => "provider_error").increment(1);
```

Run:

```bash
cargo check -p gw-adapter
```

Expected: PASS.

- [ ] **Step 8: Add wiremock provider refresh test**

Add an async test using `wiremock` that stands up a token endpoint returning:

```json
{
  "access_token": "new-access",
  "refresh_token": "new-refresh",
  "expires_in": 3600,
  "scope": "chat.completions"
}
```

The test should assert:

- The request body contains `grant_type=refresh_token`.
- The request body contains the old refresh token.
- `OAuthWriteBack.rotated_refresh_token` is `Some("new-refresh")` in a test writer.
- The returned token is `new-access`.

Run:

```bash
cargo test -p gw-adapter oauth_runtime -- --nocapture
```

Expected: PASS.

- [ ] **Step 9: Verify runtime and commit**

Run:

```bash
cargo test -p gw-adapter oauth_runtime
cargo check -p gw-adapter
git diff --check
```

Expected: PASS.

Commit:

```bash
git add crates/gw-adapter/Cargo.toml crates/gw-adapter/src/ctx.rs crates/gw-adapter/src/oauth_runtime.rs crates/gw-adapter/src/lib.rs
git commit -m "feat(oauth): add rust refresh cache and singleflight"
```

## Task W6.4: Proxy And Adapter Hook Commit

**Files:**
- Modify: `crates/gw-adapter/src/noop.rs`
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Modify: `crates/gw-gateway/src/config.rs`
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
- Test: `crates/gw-adapter/src/noop.rs`
- Test: `crates/gw-proxy/src/filters/upstream.rs`

- [ ] **Step 1: Add failing adapter hook test**

In `crates/gw-adapter/src/noop.rs`, add a test runtime that implements `OAuthRuntime::access_token_for` and returns `runtime-access`. Add a test that constructs an OAuth `VendorKey`, calls `NoopAdapter::rewrite_auth`, and asserts:

```rust
assert_eq!(headers.get("authorization"), Some("Bearer runtime-access"));
assert_eq!(runtime.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
```

Run:

```bash
cargo test -p gw-adapter noop_uses_runtime_oauth_token_before_injecting_bearer
```

Expected: FAIL because `NoopAdapter` still reads `ctx.vendor_key.oauth.access_token` directly.

- [ ] **Step 2: Hook `NoopAdapter` to `OAuthRuntime`**

In `NoopAdapter::rewrite_auth`, replace the OAuth branch with this behavior:

```rust
let auth_secret = if ctx.vendor_key.oauth.is_some() {
    let runtime = ctx.oauth_runtime.ok_or_else(|| AdapterError::Oauth(
        "oauth runtime unavailable for oauth vendor key".to_string(),
    ))?;
    let token = runtime
        .access_token_for(&ctx.vendor_key.key_id, ctx.request_id)
        .await
        .map_err(AdapterError::Oauth)?;
    if let Some(refreshed) = token.refreshed_vendor_key {
        ctx.vendor_key = refreshed;
    }
    token.access_token.expose_secret().to_owned()
} else {
    ctx.vendor_key.api_key.expose_secret().to_owned()
};
```

Run:

```bash
cargo test -p gw-adapter noop_uses_runtime_oauth_token_before_injecting_bearer
```

Expected: PASS.

- [ ] **Step 3: Carry OAuth runtime in proxy/gateway state**

Add an optional `oauth_runtime: Option<Arc<dyn gw_adapter::OAuthRuntime>>` to the relevant gateway state and request rewrite path:

- `crates/gw-proxy/src/gateway.rs::LlmGateway`
- `crates/gw-proxy/src/filters/upstream.rs::rewrite_request`
- `crates/gw-proxy/src/filters/upstream.rs::apply_outbound_adapter`

Set `AdapterCtx.oauth_runtime` from this handle instead of `None`.

Run:

```bash
cargo check -p gw-proxy
```

Expected: PASS.

- [ ] **Step 4: Build Rust OAuth runtime in gateway bootstrap**

In `crates/gw-gateway/src/config.rs`, add:

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct OAuthRefreshCfg {
    #[serde(default = "default_oauth_lock_ttl_ms")]
    pub lock_ttl_ms: u64,
    #[serde(default = "default_oauth_refresh_skew_secs")]
    pub refresh_skew_secs: i64,
    #[serde(default = "default_oauth_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "default_oauth_wait_after_lock_lost_ms")]
    pub wait_after_lock_lost_ms: u64,
}
```

Use defaults:

```rust
fn default_oauth_lock_ttl_ms() -> u64 { 30_000 }
fn default_oauth_refresh_skew_secs() -> i64 { 60 }
fn default_oauth_request_timeout_ms() -> u64 { 10_000 }
fn default_oauth_wait_after_lock_lost_ms() -> u64 { 100 }
```

In `crates/gw-gateway/src/lib.rs`, build:

```rust
let oauth_writer = Arc::new(gw_store::oauth_refresh::PgOAuthRefreshWriter::new(
    pg_pool.clone(),
    redis.clone(),
    kek_cache.clone(),
));
let oauth_runtime: Arc<dyn gw_adapter::OAuthRuntime> = Arc::new(
    gw_adapter::oauth_runtime::RustOAuthRuntime::new(
        store.clone(),
        oauth_writer,
        redis.clone(),
        reqwest::Client::new(),
        oauth_cfg,
    ),
);
```

Run:

```bash
cargo check -p gw-gateway
```

Expected: PASS.

- [ ] **Step 5: Update metrics catalog**

In `crates/gw-gateway/src/metrics_catalog.rs`, add:

```rust
MetricSpec {
    name: "gw_oauth_runtime_refresh_total",
    kind: MetricKind::Counter,
    help: "Rust OAuth runtime refresh outcomes; label=result (ok/lock_lost/cas_lost/provider_error)",
},
MetricSpec {
    name: "gw_oauth_runtime_writeback_total",
    kind: MetricKind::Counter,
    help: "Rust OAuth encrypted token write-back outcomes; label=result (ok/cas_lost/failed)",
},
```

If Option 1 is used for `oauth_failed`, replace the old metric with:

```rust
MetricSpec {
    name: "gw_oauth_refresh_status_xadd_total",
    kind: MetricKind::Counter,
    help: "OAuth status-only XADDs to events:entity after Rust-owned refresh failures",
},
```

Run:

```bash
rg -n "gw_oauth_runtime_refresh_total|gw_oauth_runtime_writeback_total|gw_oauth_refresh_status_xadd_total" crates/gw-gateway/src/metrics_catalog.rs
```

Expected: PASS with the runtime metrics present and the status metric present if Option 1 is selected.

- [ ] **Step 6: Verify proxy/adapter hook and commit**

Run:

```bash
cargo test -p gw-adapter noop oauth_runtime
cargo test -p gw-proxy oauth_injection
cargo check -p gw-gateway
git diff --check
```

Expected: PASS.

Commit:

```bash
git add crates/gw-adapter/src/noop.rs crates/gw-proxy/src/ctx.rs crates/gw-proxy/src/gateway.rs crates/gw-proxy/src/filters/upstream.rs crates/gw-gateway/src/config.rs crates/gw-gateway/src/lib.rs crates/gw-gateway/src/metrics_catalog.rs
git commit -m "feat(oauth): refresh before bearer injection"
```

## Task W6.5: OAuth Failure Event Decision Commit

**Files:**
- Modify: `crates/gw-proxy/src/error/listeners/oauth.rs`
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
- Modify: `docs/go-contracts/parts/02-go-as-consumer.md`

- [ ] **Step 1: Apply the selected decision**

For recommended Option 1, change the XADD fields in `OAuthListener` from:

```rust
("field", "oauth_failed".to_string()),
("reason", "upstream_401".to_string()),
```

to:

```rust
("field", "oauth_refresh_status".to_string()),
("status", "rust_refresh_failed".to_string()),
("reason", "upstream_401".to_string()),
("source", "rust_oauth_refresh".to_string()),
```

Change the success metric to:

```rust
metrics::counter!("gw_oauth_refresh_status_xadd_total").increment(1);
```

Run:

```bash
cargo test -p gw-proxy oauth_listener
```

Expected: FAIL until the source-grep tripwire expectations are updated.

- [ ] **Step 2: Update listener tests**

Rename `oauth_listener_emits_reverse_xadd_on_refresh_failed` to `oauth_listener_emits_status_only_xadd_on_refresh_failed`, and assert:

```rust
assert!(src.contains("oauth_refresh_status"));
assert!(src.contains("rust_refresh_failed"));
assert!(src.contains("source"));
assert!(src.contains("gw_oauth_refresh_status_xadd_total"));
assert!(!src.contains("\"oauth_failed\""));
```

Run:

```bash
cargo test -p gw-proxy oauth_listener
```

Expected: PASS.

- [ ] **Step 3: Verify old command semantics are gone**

Run:

```bash
rg -n "\"oauth_failed\"|field=oauth_failed|Go.*refresh.*oauth_failed" crates docs/go-contracts docs/spec
```

Expected: PASS only if matches are historical or explicitly marked legacy/pre-K-C. Any production Rust match to `"oauth_failed"` fails the task.

- [ ] **Step 4: Commit event decision**

```bash
git add crates/gw-proxy/src/error/listeners/oauth.rs crates/gw-gateway/src/metrics_catalog.rs docs/go-contracts/parts/02-go-as-consumer.md
git commit -m "feat(oauth): repurpose refresh failure event as status"
```

## Task W6.6: Docs And Spec Close Commit

**Files:**
- Modify: `docs/spec/07-vendor-integration.md`
- Modify: `docs/ROADMAP.md`
- Modify: `docs/plans/2026-06-09-remaining-roadmap-execution.md`

- [ ] **Step 1: Update vendor integration spec**

In `docs/spec/07-vendor-integration.md`, replace "OAuth: inject pre-decrypted token, refresh delegated to Go" with:

```markdown
### OAuth: Rust refreshes before bearer injection

OAuth credentials are decrypted at load time from `vendor_keys.payload.oauth`.
When an OAuth access token is inside the refresh skew window, the outbound adapter calls Rust's `OAuthRuntime` before injecting the bearer token.
Rust uses Redis single-flight, refreshes against `token_endpoint`, encrypts the refreshed access token and rotated refresh token when present, persists with vendor-key version CAS, and broadcasts `events:entity ns=vendor_key op=upsert`.
Go remains the OAuth configuration and initial authorization writer; Go is not a runtime refresh writer after K-C.
```

Run:

```bash
rg -n "Rust refreshes before bearer injection|Go is not a runtime refresh writer" docs/spec/07-vendor-integration.md
```

Expected: PASS.

- [ ] **Step 2: Close ROADMAP K-C item**

In `docs/ROADMAP.md`, mark K-C Rust OAuth refresh/write-back as implemented with links to:

- `crates/gw-adapter/src/oauth_runtime.rs`
- `crates/gw-store/src/oauth_refresh.rs`
- `docs/go-contracts/contract-change/2026-06-09-rust-oauth-refresh.md`

Run:

```bash
rg -n "K-C|rust-oauth-refresh|oauth_refresh.rs|oauth_runtime.rs" docs/ROADMAP.md
```

Expected: PASS with K-C no longer listed as an open capability gap.

- [ ] **Step 3: Update remaining roadmap execution plan**

In `docs/plans/2026-06-09-remaining-roadmap-execution.md`, change W6 status from planned to implemented and record the final commit SHAs from Tasks W6.1 through W6.5.

Run:

```bash
rg -n "W6: K-C Rust OAuth Refresh And Write-Back|implemented|docs\\(oauth\\)|feat\\(oauth\\)" docs/plans/2026-06-09-remaining-roadmap-execution.md
```

Expected: PASS.

- [ ] **Step 4: Full W6 verification**

Run:

```bash
cargo test -p gw-core oauth
cargo test -p gw-store oauth
cargo test -p gw-adapter oauth
cargo test -p gw-proxy oauth
cargo test -p gw-gateway kms
bash scripts/check-spec.sh docs/spec/07-vendor-integration.md
rg -n "oauth_failed|Go.*refresh|Rust.*refresh|token_endpoint" docs/go-contracts docs/spec crates
git diff --check
```

Expected:

- Cargo tests PASS.
- Spec check PASS.
- `oauth_failed` has no production Rust command semantics.
- `Go.*refresh` matches are either user-auth provider/cache sections or explicit "Go must not runtime refresh after K-C" statements.
- `Rust.*refresh` matches describe Rust runtime vendor-key refresh.
- `token_endpoint` docs say Rust reads it for vendor-key runtime refresh.

- [x] **Step 5: Commit docs close**

```bash
git add docs/spec/07-vendor-integration.md docs/ROADMAP.md docs/plans/2026-06-09-remaining-roadmap-execution.md
git commit -m "docs(oauth): close rust refresh writeback roadmap"
```

## Final Review Checklist

- [x] Contract docs say Go remains config/seed writer and Rust is the only runtime vendor-key OAuth refresh writer.
- [x] Store writer encrypts access token and rotated refresh token if present.
- [x] Store writer preserves existing encrypted refresh token when no rotated refresh token is returned.
- [x] Store writer uses version CAS and returns new version.
- [x] Store writer broadcasts `events:entity ns=vendor_key op=upsert`.
- [x] Runtime uses Redis `SET NX PX` single-flight across pods.
- [x] Lock losers and CAS losers reload the winner's token and do not report hard refresh failure.
- [x] Adapter calls Rust runtime before bearer injection.
- [x] Proxy passes real `oauth_runtime` instead of `None`.
- [x] Old `oauth_failed` Go-refresh command semantics are removed or status-only per user decision.
- [x] Metrics catalog reflects post-K-C Rust refresh outcomes.
- [x] `docs/spec/07-vendor-integration.md` no longer says refresh is delegated to Go.
- [x] `docs/ROADMAP.md` no longer lists K-C as open after implementation.
