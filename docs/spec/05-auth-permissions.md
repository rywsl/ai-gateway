# 05 — Auth & Permissions (as-built)
- Family: auth/perm · Backing crates: gw-auth, gw-proxy (request filter) · Source-of-truth: `crates/gw-auth/src/{jwt.rs,jwt_blacklist.rs,merge.rs,mgmt.rs,signature.rs,apikey.rs,model_permissions.rs}`, `crates/gw-proxy/src/filters/request.rs`, `crates/gw-core/src/entity.rs`, `config/default.toml` · Status: credential extraction (JWT / api-key pair / mgmt), key admission gates, JWT revocation blacklist, five-tier permission merge, HMAC body-signature verification (per-key + global toggle), and daily token quota are all implemented and wired through the proxy `request_filter`.

## Purpose

Resolve every inbound request to an authenticated principal and an effective permission set before any vendor I/O. The proxy `request_filter` (`crates/gw-proxy/src/filters/request.rs`) is the single admission point: it extracts a credential, verifies it, applies per-key/per-user admission gates, folds the five-tier permission lattice into one `MergedPermissions`, optionally enforces an HMAC body signature, and pre-checks the daily token quota. The `/admin/*` management channel authenticates independently of business API keys.

## As-built surface

### Credential extraction (`crates/gw-proxy/src/filters/request.rs`)
The filter reads three request headers — `authorization`, `x-api-key-id`, `x-api-key` (`request.rs:191`-`208`) — and dispatches in priority order (`request.rs:224`):

1. **`Authorization: Bearer <jwt>`** (`request.rs:224`-`299`). If a `JwtValidator` is wired (`AuthConfig.global` deployed), the hybrid validator runs (`v.validate_sub(bearer)`, `request.rs:235`); otherwise the legacy `verify_jwt(bearer, store)` async fn runs (`request.rs:265`). The hybrid validator's typed `JwtError` variants map to distinct 401 codes (`GW_AUTH_JWT_INVALID_ISS`, `GW_AUTH_JWT_INVALID_AUD`, `GW_AUTH_JWT_NOT_YET_VALID`, `GW_AUTH_JWT_EXPIRED`, `GW_AUTH_INVALID_JWT`) at `request.rs:238`-`250`.
2. **`(x-api-key-id, x-api-key)` PAIR** (`request.rs:300`). The branch matches only when *both* headers are present (`if let (Some(akid_str), Some(akey))`); a lone `x-api-key` or lone `x-api-key-id` does not satisfy it and falls through to the no-credential arm. `verify_apikey` (`crates/gw-auth/src/apikey.rs:10`) SHA-256-hashes the presented key and constant-time-compares it (`subtle::ConstantTimeEq`) against the stored `ApiKeyEntry.hash`.
3. **No credential** → 401 `GW_AUTH_INVALID_APIKEY` with message `"missing auth"` (`request.rs:395`-`407`).

Management traffic is parsed separately: `gw_auth::mgmt::parse_mgmt_authorization` (`crates/gw-auth/src/mgmt.rs:27`) accepts `Authorization: Mgmt <token>` (case-sensitive `Mgmt ` prefix, non-empty token), and `is_management_path` (`mgmt.rs:19`) gates the `/admin/` prefix. The raw bearer is split by `split_mgmt_token_payload` (`mgmt.rs:77`) into `mt_<token_id>.<secret>` halves (the `mt_` prefix is retained on `token_id`).

### Hybrid JWT validator (`crates/gw-auth/src/jwt.rs`)
- `verify_jwt` (`jwt.rs:37`): legacy path. JWKS-by-`kid` lookup via `EntityStore::jwks_or_load`, RS256 only, validates `exp` only. Returns `(UserId, Option<String> jti)`.
- `JwtValidator::validate` / `validate_sub` (`jwt.rs:191`, `jwt.rs:253`): hybrid path. Supports HS256 (`static_secret_hs256`) and RS256; RS256 key material comes from an attached `JwksResolver` keyed by `kid` (production: `EntityStoreJwksResolver`, `jwt.rs:109`) with `static_pem_rs256` fallback. Enforces `iss`/`aud`/`nbf`/`exp` with a configurable `leeway_secs` (`JwtValidatorConfig`, `jwt.rs:86`). When no `expected_aud` is configured, audience validation is explicitly disabled (`jwt.rs:241`) rather than defaulting to required. Failures classify through `classify_jwt_error` (`jwt.rs:265`) into the five token-free `JwtError` variants; `JwtError::reason()` (`jwt.rs:141`) emits a stable short tag (`iss`/`aud`/`nbf`/`exp`/`sig`) that never embeds token bytes.

### JWT revocation blacklist (`crates/gw-auth/src/jwt_blacklist.rs`)
After a successful JWT verify, if the token carries a `jti`, the filter probes the blacklist (`request.rs:280`-`298`) via `RedisJwtBlacklist::check_jwt(jti, BlacklistFailMode::FailClosed)` (`jwt_blacklist.rs:61`). The namespace is `jwt:blacklist:` (`JWT_PREFIX`, `jwt_blacklist.rs:43`). A hit yields 401 `GW_AUTH_JWT_REVOKED` (`request.rs:289`). Semantics are enforce-when-`jti`-present (tokens without a `jti` are non-revocable and pass) and fail-closed (`check_keyed` treats a backend error as revoked under `FailClosed`, `jwt_blacklist.rs:73`-`79`). A sibling `check_mgmt` method probes the isolated `mgmt_token:blacklist:` namespace (`MGMT_PREFIX`, `jwt_blacklist.rs:44`,`68`).

### Key admission gates (`crates/gw-proxy/src/filters/request.rs`)
Run on the resolved `ApiKeyEntry` before any IR parse or permission merge:
- `check_api_key_status(&entry.value)` (`request.rs:313`, defined `request.rs:1409`): exhaustive `match` over `ApiKeyStatus`; `Disabled` → 401 `GW_AUTH_KEY_DISABLED`. Checked before expiry so a disabled key reports as disabled.
- `check_api_key_expiry(&entry.value, OffsetDateTime::now_utc())` (`request.rs:330`, defined `request.rs:1392`): closed interval — `Some(exp) if now < exp` passes, so a request issued exactly at `expires_at` rejects with 401 `GW_AUTH_KEY_EXPIRED`.

`check_user_status` (`request.rs:1419`) is the parallel `Active`/`Banned` gate for user entries.

### Five-tier permission merge (`crates/gw-auth/src/merge.rs`)
`merge_permissions` (`merge.rs:28`, called `request.rs:779`) folds the lattice global → user-group → user (main account) → sub-account → api-key into one `MergedPermissions`:
- `allowed_models` / `allowed_channels` / `allowed_groups`: intersection where an *empty* set at a tier means "unrestricted, skip" (`intersect`, `merge.rs:185`).
- `rpm_limit` / `tpm_limit`: minimum positive across tiers (`min_pos`, `merge.rs:201`); `rpm` also folds in `global_rpm`.
- `markup`: product across tiers, default 1.0 (`merge.rs:121`), surfaced as `MarkupChainSnapshot` (re-exported `merge.rs:24`) with `model_markup` populated from the loaded `Pricing` entry (`merge.rs:172`).
- `forced_model_group`: highest-priority user group, ties broken by ascending `UserGroupId` (`merge.rs:133`-`142`).
- `effective_user_id`: a sub-account resolves to its `parent_id` (`merge.rs:147`); otherwise the user's own id.
- Defense-in-depth ban gate (`merge.rs:48`-`52`): a `Banned` user *or* `Banned` parent (sub-account owner) → 403 `GW_AUTH_USER_BANNED`.

### Model-permission regex with ReDoS guard (`crates/gw-auth/src/model_permissions.rs`)
`compile_pattern` (`model_permissions.rs:45`) bounds patterns at `MAX_PATTERN_LEN = 1024` bytes and caps `NFA_SIZE_LIMIT` / `DFA_SIZE_LIMIT` at build time. `check_with_budget` (`model_permissions.rs:145`, called via `check_model_whitelist_regex`, `request.rs:1525`/`803`) runs the match under a `BUDGET_MS = 100` budget; overruns call `RedosTracker::record_timeout` (`model_permissions.rs:116`), and after `TIMEOUT_THRESHOLD = 10` timeouts the ApiKey is auto-disabled (`is_disabled` → `check_with_budget` returns `None` → request denied with `model_whitelist_regex_denied_error`, `request.rs:1559`). Each timeout increments the `gw_redos_timeout_total` metric.

### HMAC request signature (`crates/gw-auth/src/signature.rs`, `crates/gw-proxy/src/filters/request.rs`)
`verify_required` (`signature.rs:88`) is the gate; the canonical signing string is `<method>\n<path>\n<timestamp>\n<sha256(body) hex>` (`signature.rs:32`), HMAC-SHA256 or HMAC-SHA512 per `SignatureAlgo` (`signature.rs:130`), compared constant-time (`signature.rs:42`). The replay window is `REPLAY_WINDOW_SECS = 300` and is symmetric — `(now - ts).abs() > 300` → `TimestampDrift` (`signature.rs:47`,`127`), i.e. ±300 s. An empty `signature_secret` while signing is required returns `SecretUnavailable` (fail-closed against the forgeable empty HMAC key, `signature.rs:104`).

The requirement is sourced from **two** inputs OR'd together (`request.rs:347`): the **global** `[signature].required` toggle (`config/default.toml:102`-`103`, struct `SignatureCfg.required` at `crates/gw-gateway/src/config.rs:277`, wired `cfg.signature.required` → `crates/gw-gateway/src/lib.rs:877`,`292` → the `request_filter` `signature_required: bool` parameter at `request.rs:66`) **plus** the per-key `entry.value.signature_required` (`ApiKeyEntry.signature_required`, `crates/gw-core/src/entity.rs:116`). When either is set, the filter stashes the algo/secret/`x-signature`/`x-timestamp` into the request context (`request.rs:347`-`364`) and runs body-after verification (`request.rs:565`-`674`) once the body is buffered, before codec parse. Outcomes map to 401 `GW_AUTH_SIGNATURE_MISSING`, 401 `GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT`, 401 `GW_AUTH_SIGNATURE_INVALID`, and a 500 `GW_AUTH_SIGNATURE_NOT_CONFIGURED` (GatewayInternal) for the empty-secret misconfiguration.

### Daily token quota (`crates/gw-proxy/src/filters/request.rs`, `crates/gw-store/src/counter_store.rs`)
This quota is resolved **Rust-side** end-to-end. The pre-check (`request.rs:1144`-`1171`) reads `UserEntry.daily_quota_tokens`; when positive it reads the counter at `daily_tokens_key(&merged.effective_user_id, &day)` = `counter:user:{eid}:daily_tokens:{date}` (`counter_store.rs:66`-`67`) and runs `check_daily_quota(consumed, freeze_est, quota)` (`request.rs:1448`): `consumed + freeze_estimate > quota` → 403 `GW_QUOTA_DAILY_TOKENS_EXCEEDED`. The day key is `day_key_utc` (`request.rs:1466`, UTC `YYYY-MM-DD`). The post-increment of the same key lives in the logging filter (`crates/gw-proxy/src/filters/logging.rs:377`, same `daily_tokens_key`). Redis read failure fails open (observational, not billing, `request.rs:1140`-`1141`); `quota == 0` short-circuits before the Redis read.

### Management tokens (`crates/gw-core/src/entity.rs`, `crates/gw-auth/src/mgmt.rs`)
`ManagementTokenEntry` (`entity.rs:567`) holds `token_id`, `user_id`, `scope: MgmtScope`, redacted `hash`, optional `expires_at`, `status: KeyStatus`, `display_name`; `Debug` is hand-written to redact `hash` (`entity.rs:579`). `MgmtScope` (`entity.rs:540`) is the lattice `AdminFull ⊇ AdminRead ⊇ UserSelf` via `covers` (`entity.rs:548`). `verify_mgmt_token_secret` (`mgmt.rs:99`) runs the constant-time SHA-256 compare *first* (timing-uniform), then status (`Active` only), expiry (`None` = no expiry), then scope, returning the `MgmtVerifyOutcome` enum (`mgmt.rs:49`). The backing table ships in migration `crates/gw-store/migrations/0007_management_tokens.sql`.

## Deliberate behaviours

- **API-key auth requires the pair.** Only `(x-api-key-id, x-api-key)` together authenticate; this is enforced by the `if let (Some, Some)` tuple match (`request.rs:300`), not by separate optional checks.
- **JWT blacklist is enforce-when-`jti`-present + fail-closed.** A `jti`-less token cannot be individually revoked and passes; a Redis outage rejects a `jti`-bearing token (`request.rs:280`, `jwt_blacklist.rs:36`-`41`,`73`-`79`).
- **Gate ordering: status before expiry before signature.** A disabled key reports `GW_AUTH_KEY_DISABLED` rather than `GW_AUTH_KEY_EXPIRED` (`request.rs:307`-`343`).
- **Expiry is a closed interval.** A request at exactly `expires_at` rejects (`request.rs:1396`-`1400`).
- **Empty-set tiers are unrestricted.** Intersection skips an empty tier rather than collapsing the result to empty (`merge.rs:185`-`199`).
- **Banned-parent disables all sub-accounts** even though the request filter loads only the direct user (`merge.rs:44`-`52`).
- **HMAC requirement is the OR of global and per-key.** The global `[signature].required` toggle forces signing for all keys; an individual key can additionally opt in (`request.rs:347`).
- **HMAC verifies the decompressed body before codec parse**, so tampered bodies never enter the IR pipeline; a decompression failure surfaces as `GW_AUTH_SIGNATURE_INVALID` (`request.rs:565`,`656`-`673`).
- **Empty HMAC secret fails closed with a 500**, not a forgeable accept (`signature.rs:104`, `request.rs:637`-`654`).
- **Daily quota fails open on Redis error** and short-circuits at `quota == 0` (`request.rs:1140`-`1149`).
- **Mgmt-token verify is timing-uniform**: the hash compare runs before status/expiry/scope so a hash mismatch is indistinguishable from later failures (`mgmt.rs:104`-`135`).

## Cross-process touchpoints

- **Redis** — JWT blacklist namespace `jwt:blacklist:` and mgmt-token namespace `mgmt_token:blacklist:` (`jwt_blacklist.rs:43`-`44`); daily-token counter `counter:user:{eid}:daily_tokens:{date}` written by the logging filter and read by the request filter (`counter_store.rs:66`, `logging.rs:377`).
- **Go control plane** — writes the `jwt:blacklist:` entries on logout / admin-revoke (consumed read-only here, `request.rs:272`-`274`); supplies JWKS PEMs loaded via `EntityStore::jwks_or_load` for the RS256 path (`jwt.rs:43`,`116`).
- **Postgres / `gw-store`** — `ApiKeyEntry`, `UserEntry`, `Pricing`, and `ManagementTokenEntry` (migration `0007_management_tokens.sql`) loaded through `EntityStore` and consumed by the gates and merge above.
- **`config/default.toml`** — the `[signature].required` global toggle flows through `gw-gateway` into the proxy filter (`config/default.toml:102`, `crates/gw-gateway/src/config.rs:277`, `crates/gw-gateway/src/lib.rs:877`).

## Invariants & lints

- `merge_permissions` re-exports `MarkupChainSnapshot` so billing and auth agree on one markup-chain type (`merge.rs:24`).
- `check_api_key_status` uses an exhaustive `match` (not `matches!`) so a new `ApiKeyStatus` variant is a compile error rather than a silent admit (`request.rs:1409`-`1414`); same for `check_user_status`.
- `day_key_utc` is the single day-boundary formatter shared by the quota pre-check and post-increment so they cannot disagree (`request.rs:1462`-`1469`, reused in `logging.rs`).
- Mgmt and JWT blacklist namespaces are isolated by distinct Redis prefixes; a source-level test asserts both prefixes and both `check_*` methods exist (`jwt_blacklist.rs:110`-`128`).
- `JwtError::reason()` guarantees logs/metrics carry only a coarse tag, never raw token bytes (`jwt.rs:141`).

## Not built → ROADMAP

No open ROADMAP items. The prior `no_global_hmac_force` invariant was retired on 2026-06-08 because `[signature].required` is an intentional built feature, not a forbidden global force path.
