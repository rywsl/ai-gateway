//! `request_filter` — auth, parse, permission merge, rate-limit, hard caps,
//! freeze-estimate, USER_CIRCUIT check.
//!
//! Each step on failure constructs an `ErrorIR`, renders it through the
//! client codec via [`error_emit::send_error`], and returns `Ok(true)` to
//! tell Pingora the request is fully handled (no upstream).
//!
//! On the happy path we return `Ok(false)` so Pingora continues to the
//! `upstream_peer` phase.

use crate::ctx::ReqCtx;
use crate::error_emit;
use crate::filters::early::{
    client_ip_from_session, pick_emit_codec, short_circuit, with_request_id,
};
use gw_auth::apikey::{resolve_apikey_by_secret, verify_apikey};
use gw_auth::ip_allowlist::{IpAllowlistGuard, IpAllowlistMode};
use gw_auth::ip_filter::check_ip;
use gw_auth::ip_ratelimit::IpRateLimitGuard;
use gw_auth::jwt::{verify_jwt, JwtError, JwtValidator};
use gw_auth::jwt_blacklist::RedisJwtBlacklist;
use gw_auth::merge_permissions;
use gw_auth::model_permissions::{
    check_with_budget, ModelPermission, PermissionEffect, RedosTracker,
};
use gw_auth::ratelimit::RateLimitGuard;
use gw_auth::AuthCtx;
use gw_billing::TaskBillingConfig;
use gw_codec::cli::CliCodec;
use gw_codec::gemini::files::GeminiFilesGateway;
use gw_core::entity::{ApiKeyEntry, EstimatePricing};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{ApiKeyId, ModelName, UserId};
use gw_core::protocol::ApiShape;
use gw_core::versioned::Versioned;
use gw_store::counter_store::daily_tokens_key;
use gw_store::deduction::{DeductionState, UserCircuit, DEDUCT, USER_CIRCUIT};
use gw_store::{CounterStore, EntityStore};
use pingora::Result;
use pingora_proxy::Session;
use std::borrow::Cow;
use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use time::OffsetDateTime;

pub struct GeminiFilesRoutingDeps<'a> {
    pub store: &'a Arc<EntityStore>,
    pub health: &'a Arc<gw_health::KeyHealthStore>,
    pub in_flight: &'a Arc<gw_routing::in_flight::InFlightStore>,
    pub strategies: &'a Arc<gw_routing::strategy::set::StrategySet>,
    pub latency_tracker: &'a Arc<gw_routing::LatencyTracker>,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    ratelimit: &Arc<RateLimitGuard>,
    ip_allowlist: &Arc<IpAllowlistGuard>,
    ip_ratelimit: &Arc<IpRateLimitGuard>,
    ip_allowlist_mode: IpAllowlistMode,
    counters: &Arc<CounterStore>,
    global_rpm: u32,
    trust_xff: bool,
    trusted_proxies: &Arc<Vec<ipnet::IpNet>>,
    signature_required: bool,
    signature_replay: &Arc<gw_auth::signature::ReplayChecker>,
    task_enabled: bool,
    task_contract: Option<&gw_task::redis_contract::RedisTaskContract>,
    task_billing_config: Option<TaskBillingConfig>,
    jwt_validator: Option<&Arc<JwtValidator>>,
    capability_resolver: Option<&Arc<gw_capability::CapabilityResolver>>,
    mgmt_blacklist: &Arc<RedisJwtBlacklist>,
    hard_cap_cfg: &gw_auth::hard_cap::HardCapCfg,
    protection_cfg: &gw_ledger::ProtectionConfig,
    overflow_gate: &Arc<gw_ledger::overflow::OverflowGate>,
    active_registration: &gw_ledger::ActiveRegistrationHandle,
    files_gateway: &dyn GeminiFilesGateway,
    routing_deps: Option<GeminiFilesRoutingDeps<'_>>,
) -> Result<bool> {
    ctx.phase_start("request_filter");
    ctx.timing.start = Some(Instant::now());

    // ---------------------------------------------------------------------
    // 0. Management-plane path branch (G-α.9 / G-β wire).
    //    `/admin/*` requests are destined for the management plane, not the
    //    LLM business path. We tag `ctx.mgmt = true` and reject any request
    //    that lacks a `Mgmt <token>` Authorization header. The full
    //    mgmt-token validation against `entity:management_token` is a future
    //    spec — v1 just rejects unauthorized mgmt requests cleanly rather
    //    than mixing them with the business ApiKey/JWT code paths below.
    // ---------------------------------------------------------------------
    let req_path = session.req_header().uri.path().to_string();
    let req_method = session.req_header().method.as_str().to_owned();
    if let Some(action) =
        crate::filters::gemini_files::detect_gemini_files_route(&req_method, &req_path)
    {
        ctx.is_gemini_files = true;
        ctx.gemini_files_action = Some(action);
    }
    let req_content_type = session
        .req_header()
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if !ctx.is_task {
        crate::filters::task::mark_task_context(
            ctx,
            task_enabled,
            &req_method,
            &req_path,
            req_content_type.as_deref(),
        );
    }
    if gw_auth::mgmt::is_management_path(&req_path) {
        ctx.mgmt = true;
        let req_id_for_mgmt = ctx.request_id.0.clone();
        let mgmt_auth_hdr = session
            .req_header()
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        macro_rules! mgmt_bail {
            ($outcome:expr, $internal_outcome:expr) => {{
                tracing::warn!(
                    target: "gw_proxy::mgmt_auth",
                    outcome = ?$outcome,
                    internal_outcome = $internal_outcome,
                    "management token authentication failed"
                );
                let (status, code) = gw_auth::mgmt::public_error_for_outcome(&$outcome)
                    .expect("mgmt_bail requires non-ok outcome");
                let err = ErrorIR {
                    category: ErrorCategory::ClientAuth,
                    status,
                    upstream_status: None,
                    code: Cow::Borrowed(code),
                    message: Cow::Borrowed(""),
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                emit_and_bail(session, ctx, &err, &req_id_for_mgmt).await?;
                unreachable!()
            }};
        }

        // 1. Authorization: Mgmt <token>
        let Some(parsed_auth) = gw_auth::mgmt::parse_mgmt_authorization(mgmt_auth_hdr) else {
            mgmt_bail!(
                gw_auth::mgmt::MgmtVerifyOutcome::InvalidFormat,
                "missing_authorization"
            );
        };

        // 2. Split mt_<id>.<secret>
        let Some(parts) = gw_auth::mgmt::split_mgmt_token_payload(&parsed_auth.token) else {
            mgmt_bail!(
                gw_auth::mgmt::MgmtVerifyOutcome::InvalidFormat,
                "invalid_format"
            );
        };

        // 3. Load entry via Moka L1 → cold load. Miss → INVALID (same code as
        //    hash mismatch, by design — see spec §1.3 step 4).
        let Some(entry) = store.mgmt_token_or_load(parts.token_id).await else {
            mgmt_bail!(gw_auth::mgmt::MgmtVerifyOutcome::Invalid, "not_found");
        };

        // 4. Mgmt-token blacklist. FailClosed: a Redis outage rejects the
        //    request rather than serving stale-allow.
        if mgmt_blacklist
            .check_mgmt(
                parts.token_id,
                gw_auth::jwt_blacklist::BlacklistFailMode::FailClosed,
            )
            .await
        {
            mgmt_bail!(gw_auth::mgmt::MgmtVerifyOutcome::Disabled, "revoked");
        }

        // 5. Constant-time secret compare + status / expiry / scope.
        //    Gateway requires AdminRead at minimum; fine-grained admin-full vs
        //    admin-read splits are enforced by the Go control plane.
        match gw_auth::mgmt::verify_mgmt_token_secret(
            &entry,
            parts.secret,
            gw_core::entity::MgmtScope::AdminRead,
        ) {
            gw_auth::mgmt::MgmtVerifyOutcome::Ok => {
                ctx.mgmt_user_id = Some(entry.value.user_id.clone());
                ctx.mgmt_scope = Some(entry.value.scope);
            }
            gw_auth::mgmt::MgmtVerifyOutcome::InvalidFormat
            | gw_auth::mgmt::MgmtVerifyOutcome::Invalid => {
                mgmt_bail!(gw_auth::mgmt::MgmtVerifyOutcome::Invalid, "invalid_secret");
            }
            gw_auth::mgmt::MgmtVerifyOutcome::Expired => {
                mgmt_bail!(gw_auth::mgmt::MgmtVerifyOutcome::Expired, "expired");
            }
            gw_auth::mgmt::MgmtVerifyOutcome::Disabled => {
                mgmt_bail!(gw_auth::mgmt::MgmtVerifyOutcome::Disabled, "disabled");
            }
            gw_auth::mgmt::MgmtVerifyOutcome::ScopeDenied => {
                mgmt_bail!(
                    gw_auth::mgmt::MgmtVerifyOutcome::ScopeDenied,
                    "scope_denied"
                );
            }
        }

        // Token verified; ctx.mgmt_user_id / mgmt_scope are populated.
        // Continue to `upstream_peer`, which routes management traffic to the
        // Go control plane instead of the LLM vendor path below.
        return Ok(false);
    }

    // ---------------------------------------------------------------------
    // 1. Auth — Bearer JWT or `x-api-key` header.
    // ---------------------------------------------------------------------
    let auth_header = session
        .req_header()
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let api_key_id_hdr = session
        .req_header()
        .headers
        .get("x-api-key-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let api_key_hdr = session
        .req_header()
        .headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let request_id = ctx.request_id.0.clone();

    // Emit an error response to the client, then short-circuit the filter chain.
    //
    // Wraps `emit_and_bail` + `unreachable!()` — the `unreachable!()` is centralised
    // here rather than repeated at every call site. `emit_and_bail` always returns
    // `Err`, so the `?` operator unwinds before `unreachable!()` is reached.
    macro_rules! bail {
        ($session:expr, $ctx:expr, $err:expr, $rid:expr) => {{
            emit_and_bail($session, $ctx, $err, $rid).await?;
            unreachable!("emit_and_bail always returns Err; see fn doc")
        }};
    }

    let bearer_token = auth_header
        .as_deref()
        .and_then(|h| h.strip_prefix("Bearer "));
    let single_api_key = single_api_key_secret(
        bearer_token,
        api_key_hdr.as_deref(),
        api_key_id_hdr.as_deref(),
    );
    let mut auth_is_jwt = false;
    let mut jwt_token_version: Option<u64> = None;

    let user_id_res: Result<(UserId, Option<ApiKeyId>, Option<Vec<String>>)> = if let Some(akey) =
        single_api_key
    {
        match resolve_apikey_by_secret(akey, store).await {
            Ok(entry) => {
                let akid = entry.value.key_id.clone();
                match resolve_api_key_entry(session, ctx, &entry, akid, signature_required) {
                    Ok(scope) => Ok(scope),
                    Err(err) => bail!(session, ctx, &err, &request_id),
                }
            }
            Err(err) => {
                bail!(session, ctx, &err, &request_id)
            }
        }
    } else if let Some(bearer) = bearer_token {
        // G6 (T1-E): if AuthConfig.global is wired into the gateway, use the
        // hybrid validator (JWKS resolver + iss/aud/nbf/leeway enforcement
        // with HS256 / static_pem_rs256 fallbacks). Otherwise fall through to
        // the legacy `verify_jwt` so deployments without AuthConfig keep
        // working with v1 semantics (exp-only, JWKS-by-kid).
        let (jwt_uid, jwt_jti, jwt_tv): (UserId, Option<String>, Option<u64>) = match jwt_validator
        {
            Some(v) => match v.validate_sub(bearer).await {
                Ok(triple) => triple,
                Err(jerr) => {
                    let (code, msg) = match jerr {
                        JwtError::InvalidIss => {
                            (codes::GW_AUTH_JWT_INVALID_ISS, "jwt issuer invalid")
                        }
                        JwtError::InvalidAud => {
                            (codes::GW_AUTH_JWT_INVALID_AUD, "jwt audience invalid")
                        }
                        JwtError::NotYetValid => {
                            (codes::GW_AUTH_JWT_NOT_YET_VALID, "jwt not yet valid")
                        }
                        JwtError::Expired => (codes::GW_AUTH_JWT_EXPIRED, "jwt expired"),
                        JwtError::InvalidSignature => (codes::GW_AUTH_INVALID_JWT, "jwt invalid"),
                    };
                    let err = ErrorIR {
                        category: ErrorCategory::ClientAuth,
                        status: 401,
                        upstream_status: None,
                        code: Cow::Borrowed(code),
                        message: Cow::Borrowed(msg),
                        retryable: false,
                        upstream_raw: None,
                        request_id: "",
                        retry_after_secs: None,
                    };
                    bail!(session, ctx, &err, &request_id)
                }
            },
            None => match verify_jwt(bearer, store).await {
                Ok(triple) => triple,
                Err(err) => {
                    bail!(session, ctx, &err, &request_id)
                }
            },
        };
        auth_is_jwt = true;
        jwt_token_version = jwt_tv;
        // Revocation gate (audit 2026-05-30): a client JWT carrying a `jti`
        // present in the `jwt:blacklist:` namespace (written by Go on
        // logout/admin-revoke) is rejected even though it is still within `exp`.
        // Enforce-when-present: the blacklist is `jti`-keyed, so a token minted
        // without a `jti` is inherently non-revocable and passes — checking
        // only when `jti` is present closes the revocable-token hole without
        // breaking IdPs that omit `jti`. FailClosed: a Redis outage rejects a
        // `jti`-bearing token (contract go-contracts/parts/01 §1.6 default).
        if let Some(jti) = jwt_jti.as_deref() {
            if mgmt_blacklist
                .check_jwt(jti, gw_auth::jwt_blacklist::BlacklistFailMode::FailClosed)
                .await
            {
                let err = ErrorIR {
                    category: ErrorCategory::ClientAuth,
                    status: 401,
                    upstream_status: None,
                    code: Cow::Borrowed(codes::GW_AUTH_JWT_REVOKED),
                    message: Cow::Borrowed("jwt revoked"),
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &err, &request_id)
            }
        }
        Ok((jwt_uid, None, None))
    } else if let (Some(akid_str), Some(akey)) = (api_key_id_hdr.as_ref(), api_key_hdr.as_ref()) {
        let akid = ApiKeyId::from(akid_str.as_str());
        match verify_apikey(akey, &akid, store).await {
            Ok(()) => {
                // Resolve the user_id via the api-key entry, then run the same
                // disabled/expired/signature gates as the single-secret shapes
                // through `resolve_api_key_entry` so the two paths cannot drift.
                match store.apikey_or_load(&akid).await {
                    Some(entry) => {
                        match resolve_api_key_entry(session, ctx, &entry, akid, signature_required)
                        {
                            Ok(scope) => Ok(scope),
                            Err(err) => bail!(session, ctx, &err, &request_id),
                        }
                    }
                    None => {
                        let err = ErrorIR {
                            category: ErrorCategory::ClientAuth,
                            status: 401,
                            upstream_status: None,
                            code: Cow::Borrowed(codes::GW_AUTH_INVALID_APIKEY),
                            message: Cow::Borrowed("api key not found"),
                            retryable: false,
                            upstream_raw: None,
                            request_id: "",
                            retry_after_secs: None,
                        };
                        bail!(session, ctx, &err, &request_id)
                    }
                }
            }
            Err(err) => {
                bail!(session, ctx, &err, &request_id)
            }
        }
    } else {
        let err = ErrorIR {
            category: ErrorCategory::ClientAuth,
            status: 401,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_AUTH_INVALID_APIKEY),
            message: Cow::Borrowed("missing auth"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        };
        bail!(session, ctx, &err, &request_id)
    };
    let (user_id, api_key_id, ip_allowlist_for_request) = user_id_res?;

    if apikey_module_blocks(
        api_key_id.as_ref(),
        store.module_enabled_or_load("apikey").await,
    ) {
        let err = ErrorIR {
            category: ErrorCategory::ClientAuth,
            status: 403,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_AUTH_MODULE_DISABLED),
            message: Cow::Borrowed("api key module disabled"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        };
        bail!(session, ctx, &err, &request_id)
    }

    // Load the user entry (used by ReqCtx for downstream consumers).
    let user_entry = match store.user_or_load(&user_id).await {
        Some(u) => u,
        None => {
            let err = ErrorIR {
                category: ErrorCategory::ClientAuth,
                status: 401,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_AUTH_USER_NOT_FOUND),
                message: Cow::Borrowed("user not found"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        }
    };
    // Admit-time status gate (audit 2026-05-30): reject banned users. Like the
    // disabled-key gate above, the loader only filters deleted_at, so a row
    // flipped to Banned via op=status_change loads normally and would otherwise
    // be served. Parent (sub-account owner) banning is enforced in
    // merge_permissions, which loads the parent.
    if check_user_status(&user_entry.value).is_err() {
        let err = ErrorIR {
            category: ErrorCategory::ClientAuth,
            status: 403,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_AUTH_USER_BANNED),
            message: Cow::Borrowed("user banned"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        };
        bail!(session, ctx, &err, &request_id)
    }
    if auth_is_jwt
        && check_token_version(jwt_token_version, user_entry.value.token_version).is_err()
    {
        let err = ErrorIR {
            category: ErrorCategory::ClientAuth,
            status: 401,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_AUTH_JWT_REVOKED),
            message: Cow::Borrowed("jwt revoked"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        };
        bail!(session, ctx, &err, &request_id)
    }
    ctx.user = Some(user_entry);
    ctx.api_key_id = api_key_id.clone();

    // T6-C: parse Content-Encoding for the request body so the
    // request_body_filter (or `body_verify::verify_body`) knows whether to
    // decode before signature verification + codec parse.
    let enc_header = session
        .req_header()
        .headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok());
    ctx.req_content_encoding = gw_codec::compression::parse_content_encoding(enc_header);

    // ---------------------------------------------------------------------
    // 2. Re-run unified IP filtering with the resolved scope IDs.
    // ---------------------------------------------------------------------
    let client_ip_v = client_ip_from_session(session, trust_xff, trusted_proxies);
    if let Some(ip) = client_ip_v {
        if let Err(err) = check_ip(
            ip,
            Some(&user_id),
            api_key_id.as_ref(),
            store,
            ip_allowlist,
            ip_ratelimit,
            ip_allowlist_mode,
        )
        .await
        {
            bail!(session, ctx, &err, &request_id)
        }
    }

    // ---------------------------------------------------------------------
    // 2b. G2 — per-key IP allowlist (after the User/ApiKey-scoped blocklist).
    //     `None` and empty list mean unrestricted; otherwise the client IP
    //     must match at least one CIDR. Malformed entries fail closed: they
    //     are skipped + a warn is logged inside `match_any_cidr`.
    // ---------------------------------------------------------------------
    if let Some(ip) = client_ip_v {
        if check_api_key_ip_allowlist(ip_allowlist_for_request.as_deref(), ip).is_err() {
            let err = ErrorIR {
                category: ErrorCategory::ClientAuth,
                status: 403,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_AUTH_KEY_IP_NOT_ALLOWED),
                message: Cow::Borrowed("client ip not in api key allowlist"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        }
    }

    // ---------------------------------------------------------------------
    // 3. Read body (cap at 16 MiB to bound memory under abuse).
    // ---------------------------------------------------------------------
    const BODY_CAP: usize = 16 * 1024 * 1024;
    let mut body_buf: Vec<u8> = Vec::new();
    // `request_filter` consumes the whole downstream body here, before Pingora's
    // proxy loop forwards the request upstream. Without retry buffering Pingora
    // retains no copy, so it sends the upstream headers (incl. Content-Length)
    // with an empty body and the upstream blocks waiting for bytes that never
    // arrive (observed as a ~30s upstream ReadTimedout). Enabling retry buffering
    // before the first read makes Pingora keep the body and replay it upstream,
    // where `request_body_filter` swaps in the rewritten body at end-of-stream.
    session.as_mut().enable_retry_buffering();
    while let Some(chunk) = session.read_request_body().await? {
        if body_buf.len() + chunk.len() > BODY_CAP {
            let err = ErrorIR {
                category: ErrorCategory::ClientBadRequest,
                status: 413,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_REQUEST_INPUT_TOO_LARGE),
                message: Cow::Borrowed("body exceeds 16 MiB"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        }
        body_buf.extend_from_slice(&chunk);
    }
    let body_bytes = bytes::Bytes::from(body_buf);
    ctx.body = Some(crate::body::RequestBodyView::from_raw(
        body_bytes.clone(),
        ctx.req_content_encoding,
    ));
    if !ctx.is_task {
        crate::filters::task::mark_task_context(
            ctx,
            task_enabled,
            &req_method,
            &req_path,
            req_content_type.as_deref(),
        );
    }

    // Wire-fidelity archive: snapshot the raw downstream request. The body
    // view's outbound_or_raw() may be overwritten by upstream::rewrite_request
    // on the cross-protocol path, so the snapshot must happen now.
    ctx.downstream_request_body = Some(Arc::new(body_bytes.clone()));
    {
        use crate::ctx::DownstreamRequestMeta;
        let req = session.req_header();
        ctx.downstream_request_meta = Some(DownstreamRequestMeta {
            method: req.method.as_str().to_owned(),
            url: req.uri.to_string(),
            http_version: format!("{:?}", req.version),
            headers: req
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
                .collect(),
        });
    }

    // ---------------------------------------------------------------------
    // 3b. G7 / T1-D — HMAC body-after verification. If the resolved api-key
    //     requires a signature, verify `x-signature` + `x-timestamp` against
    //     the (decompressed) body now that the full body is buffered. This
    //     runs BEFORE codec parse so tampered bodies never enter the IR
    //     pipeline. Three distinct 401 codes are emitted per ADR-G7-1.
    // ---------------------------------------------------------------------
    if ctx.signature_required {
        use gw_auth::signature::SignatureCheckResult;
        let method = session.req_header().method.as_str().to_owned();
        let path = session.req_header().uri.path().to_owned();
        let input = crate::body_verify::BodyVerifyInput {
            method: &method,
            path: &path,
            body: body_bytes.clone(),
            content_encoding: ctx.req_content_encoding,
            signature_required: ctx.signature_required,
            signature_algo: ctx.signature_algo,
            secret: &ctx.signature_secret,
            signature: ctx.signature_received.clone(),
            timestamp: ctx.signature_timestamp.clone(),
            max_decoded_bytes: Some(32 * 1024 * 1024),
        };
        match crate::body_verify::verify_body(input) {
            Ok((SignatureCheckResult::Valid, decoded)) => {
                if let Err(err) = signature_replay.check(ctx.signature_nonce.as_deref()).await {
                    let (category, status, code, message) = match err {
                        gw_auth::signature::ReplayCheckError::MissingNonce
                        | gw_auth::signature::ReplayCheckError::ReplayDetected => (
                            ErrorCategory::ClientAuth,
                            401,
                            codes::GW_AUTH_REPLAY_DETECTED,
                            "signature replay detected",
                        ),
                        gw_auth::signature::ReplayCheckError::Backend(_) => (
                            ErrorCategory::GatewayInternal,
                            503,
                            codes::GW_BACKEND_UNAVAILABLE,
                            "signature replay check unavailable",
                        ),
                    };
                    let err = ErrorIR {
                        category,
                        status,
                        upstream_status: None,
                        code: Cow::Borrowed(code),
                        message: Cow::Borrowed(message),
                        retryable: false,
                        upstream_raw: None,
                        request_id: "",
                        retry_after_secs: None,
                    };
                    bail!(session, ctx, &err, &request_id)
                }
                if !matches!(
                    ctx.req_content_encoding,
                    gw_codec::compression::Encoding::Identity
                ) {
                    // Pre-populate view's decoded OnceCell so codec parse reuses
                    // the bytes HMAC already decompressed — decompress happens once.
                    if let Some(view) = ctx.body.as_ref() {
                        view.prefill_decoded(decoded);
                    }
                }
            }
            Ok((SignatureCheckResult::NotRequired, decoded)) => {
                if !matches!(
                    ctx.req_content_encoding,
                    gw_codec::compression::Encoding::Identity
                ) {
                    // Pre-populate view's decoded OnceCell so codec parse reuses
                    // the bytes HMAC already decompressed — decompress happens once.
                    if let Some(view) = ctx.body.as_ref() {
                        view.prefill_decoded(decoded);
                    }
                }
            }
            Ok((SignatureCheckResult::Missing, _)) => {
                let err = ErrorIR {
                    category: ErrorCategory::ClientAuth,
                    status: 401,
                    upstream_status: None,
                    code: Cow::Borrowed(codes::GW_AUTH_SIGNATURE_MISSING),
                    message: Cow::Borrowed("signature or timestamp header missing"),
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &err, &request_id)
            }
            Ok((SignatureCheckResult::TimestampDrift, _)) => {
                let err = ErrorIR {
                    category: ErrorCategory::ClientAuth,
                    status: 401,
                    upstream_status: None,
                    code: Cow::Borrowed(codes::GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT),
                    message: Cow::Borrowed("signature timestamp drifted"),
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &err, &request_id)
            }
            Ok((SignatureCheckResult::Invalid, _)) => {
                let err = ErrorIR {
                    category: ErrorCategory::ClientAuth,
                    status: 401,
                    upstream_status: None,
                    code: Cow::Borrowed(codes::GW_AUTH_SIGNATURE_INVALID),
                    message: Cow::Borrowed("signature invalid"),
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &err, &request_id)
            }
            Ok((SignatureCheckResult::SecretUnavailable, _)) => {
                // Fail-closed: signing required but no signature_secret configured.
                // An empty HMAC key is forgeable, so reject as a server misconfig
                // (500) rather than verifying against it. GatewayInternal, not
                // ClientAuth — the client did nothing wrong; ops must populate the
                // secret (DB-20) before enabling HMAC for this key.
                let err = ErrorIR {
                    category: ErrorCategory::GatewayInternal,
                    status: 500,
                    upstream_status: None,
                    code: Cow::Borrowed(codes::GW_AUTH_SIGNATURE_NOT_CONFIGURED),
                    message: Cow::Borrowed("signature required but no secret configured"),
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &err, &request_id)
            }
            Err(_decompress_err) => {
                // Body decompression failed: signature is computed over
                // plaintext, so we cannot verify. No dedicated body-decode
                // error code exists in v1; surface as signature-invalid to
                // keep the rejection in a single ClientAuth code family.
                let err = ErrorIR {
                    category: ErrorCategory::ClientAuth,
                    status: 401,
                    upstream_status: None,
                    code: Cow::Borrowed(codes::GW_AUTH_SIGNATURE_INVALID),
                    message: Cow::Borrowed("signature invalid (body decompression failed)"),
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &err, &request_id)
            }
        }
    }

    // ---------------------------------------------------------------------
    // 4. Parse the request through the client codec into IR. The IR is
    //    inspected for model name + stream flag; we then drop the parsed
    //    IR (its borrows would conflict with mutating `ctx`).
    // ---------------------------------------------------------------------
    let cli_shape = ctx.client_shape.filter(ApiShape::is_cli);
    let codec = match ctx.client_codec.as_ref() {
        Some(c) => Some(c),
        None if ctx.is_task => None,
        None => {
            // Should never happen — early_request_filter sets this.
            let err = ErrorIR {
                category: ErrorCategory::GatewayInternal,
                status: 500,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
                message: Cow::Borrowed("no client codec"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        }
    };
    let parse_body = {
        let Some(view) = ctx.body.as_ref() else {
            let err = ErrorIR {
                category: ErrorCategory::GatewayInternal,
                status: 500,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
                message: Cow::Borrowed("request body unavailable after buffering"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        };
        match view.decoded(32 * 1024 * 1024) {
            Ok(b) => b.clone(),
            Err(_) => {
                // Decompression failed; fall back to raw bytes for codec
                // (which will likely fail parse and emit a clean 400).
                // This path is rare: normally the HMAC block rejects bad
                // compressed bodies before we get here.
                view.raw().clone()
            }
        }
    };
    let path_str = session.req_header().uri.path();
    let openai_task_client_request = if ctx
        .task_endpoint_candidate
        .as_ref()
        .is_some_and(crate::filters::task::should_parse_openai_compatible_client_request)
    {
        let Some(candidate) = ctx.task_endpoint_candidate.as_ref() else {
            let err = ErrorIR {
                category: ErrorCategory::GatewayInternal,
                status: 500,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
                message: Cow::Borrowed("task endpoint candidate missing"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        };
        match crate::filters::task::parse_openai_compatible_client_request(
            candidate,
            req_content_type.as_deref(),
            &parse_body,
        )
        .await
        {
            Ok(value) => Some(value),
            Err(err) => {
                let error_ir = task_error_ir(err);
                bail!(session, ctx, &error_ir, &request_id)
            }
        }
    } else {
        None
    };
    let openai_task_request_key = if let (Some(candidate), Some(client_request)) = (
        ctx.task_endpoint_candidate.as_ref(),
        openai_task_client_request.as_ref(),
    ) {
        match crate::filters::task::resolve_task_request_key(candidate, client_request) {
            Ok(key) => Some(key),
            Err(err) => {
                let error_ir = task_error_ir(err);
                bail!(session, ctx, &error_ir, &request_id)
            }
        }
    } else {
        None
    };
    let (model_name, is_stream, est_input_tokens, req_max_tokens) = if ctx.is_gemini_files {
        (ModelName::from("gemini-files"), false, 1, None)
    } else if let Some(key) = openai_task_request_key.as_ref() {
        (
            ModelName::from(key.global_model_id.as_str()),
            false,
            1,
            None,
        )
    } else if ctx.is_task {
        let model = if matches!(
            ctx.task_action,
            Some(crate::filters::task::TaskAction::Submit)
        ) {
            match serde_json::from_slice::<serde_json::Value>(&parse_body)
                .ok()
                .and_then(|value| {
                    value
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .filter(|value| !value.is_empty())
            {
                Some(model) => model,
                None => {
                    let err = ErrorIR {
                        category: ErrorCategory::ClientBadRequest,
                        status: 400,
                        upstream_status: None,
                        code: Cow::Borrowed(codes::GW_TASK_MODEL_REQUIRED),
                        message: Cow::Borrowed("model is required for task submit"),
                        retryable: false,
                        upstream_raw: None,
                        request_id: "",
                        retry_after_secs: None,
                    };
                    bail!(session, ctx, &err, &request_id)
                }
            }
        } else {
            "task".to_string()
        };
        (ModelName::from(model.as_str()), false, 1, None)
    } else if let Some(shape) = cli_shape {
        let hints = extract_cli_request_hints(shape, path_str, Some(&parse_body));
        let model = hints.model.unwrap_or_default();
        let is_stream = hints.stream.unwrap_or(false);
        (ModelName::from(model.as_str()), is_stream, 1, None)
    } else {
        let Some(codec) = codec else {
            let err = ErrorIR {
                category: ErrorCategory::GatewayInternal,
                status: 500,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
                message: Cow::Borrowed("no client codec"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        };
        let parsed = codec.parse_request(&parse_body);
        let (model_name, is_stream, est_input_tokens, max_tokens) = match parsed {
            Ok(ir) => {
                let values = (
                    ModelName::from(ir.model.as_ref()),
                    ir.stream,
                    codec.estimate_input_tokens(&ir),
                    ir.max_tokens,
                );
                values
            }
            Err(err) => {
                bail!(session, ctx, &err, &request_id)
            }
        };
        let is_stream = is_stream || codec.path_implies_stream(path_str);
        let model_name = if model_name.as_ref().is_empty() {
            codec
                .extract_path_model(path_str)
                .map(ModelName::from)
                .unwrap_or(model_name)
        } else {
            model_name
        };
        (model_name, is_stream, est_input_tokens, max_tokens)
    };
    ctx.model = Some(model_name.clone());
    ctx.is_stream = is_stream;
    // POSTs that mutate vendor state are *not* idempotent. We can refine
    // this once the codec exposes "method classification"; non-stream
    // completions are idempotent, stream/tool requests are not.
    ctx.idempotent = !is_stream;

    // ---------------------------------------------------------------------
    // 4c. G5 (T2-B) — pre-load Pricing for the resolved model now that
    //     `model_name` is known. The EntityStore is Moka-memoised so a later
    //     fetch in `upstream_peer` is a hit; pulling the load forward lets
    //     `merge_permissions` populate `MarkupChainSnapshot.model_markup`
    //     instead of leaving the 1.0 placeholder. A missing Pricing entry
    //     (e.g. unconfigured model) falls through with `None` so the chain
    //     entry stays at the multiplicative-identity 1.0.
    // ---------------------------------------------------------------------
    let pricing = store.pricing_or_load(&model_name).await;
    ctx.pricing = pricing.clone();
    let estimate_pricing = store.estimate_pricing_or_load(&model_name).await;

    // ---------------------------------------------------------------------
    // 5. Merge permissions across the five-tier hierarchy.
    // ---------------------------------------------------------------------
    let auth_ctx = AuthCtx {
        user_id: user_id.clone(),
        api_key_id: api_key_id.clone(),
        client_ip: client_ip_v.unwrap_or(IpAddr::from([0, 0, 0, 0])),
    };
    let merged = match merge_permissions(&auth_ctx, store, global_rpm, pricing.clone()).await {
        Ok(p) => p,
        Err(err) => {
            bail!(session, ctx, &err, &request_id)
        }
    };

    // ---------------------------------------------------------------------
    // 6. Model permission check.
    // ---------------------------------------------------------------------
    let enforce_model_permissions = task_request_requires_model_permission(ctx);
    if enforce_model_permissions
        && !merged.allowed_models.is_empty()
        && !merged.allowed_models.contains(&model_name)
    {
        let err = ErrorIR {
            category: ErrorCategory::ClientAuth,
            status: 403,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_REQUEST_MODEL_NOT_FOUND),
            message: Cow::Borrowed("model not in allowed_models"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        };
        bail!(session, ctx, &err, &request_id)
    }
    if enforce_model_permissions {
        if let Err(err) = check_model_whitelist_regex(
            &model_name,
            merged.model_whitelist_regex.as_deref(),
            api_key_id.as_ref().map(ApiKeyId::as_ref),
            model_regex_redos_tracker(),
            &ctx.regex_cache,
        ) {
            bail!(session, ctx, &err, &request_id)
        }
    }

    // ---------------------------------------------------------------------
    // 6b. G13 (ADR-G13-2) — capability resolve. After permission merge and the
    //     model-permission check, collapse (ApiKey forced caps + Model caps +
    //     `x-require-capability` header) into a single `RequiredCapabilities`
    //     set and stash it on `ctx.required_caps`. `upstream_peer` then passes
    //     it into `get_candidate_keys`, which capability-filters each candidate
    //     channel. `None` resolver (deployment without Model entities) leaves
    //     `ctx.required_caps` empty so routing is unconstrained.
    // ---------------------------------------------------------------------
    if let Some(resolver) = capability_resolver {
        let header_require: Vec<String> = session
            .req_header()
            .headers
            .get_all("x-require-capability")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|s| s.split(',').map(str::trim))
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let header_refs: Vec<&str> = header_require.iter().map(String::as_str).collect();
        let protocol = ctx
            .client_codec
            .as_ref()
            .map(|c| c.kind())
            .unwrap_or(gw_core::protocol::ProtocolKind::OpenAi);
        let req = gw_capability::ResolveRequest {
            api_key_id: api_key_id.as_ref().map(|a| a.as_ref()).unwrap_or(""),
            model_alias: model_name.as_ref(),
            header_require: if header_refs.is_empty() {
                None
            } else {
                Some(header_refs.as_slice())
            },
            protocol,
            retry_upgrade: ctx.retry_upgrade_hint.as_ref(),
        };
        match resolver.resolve(&req).await {
            Ok(caps) => ctx.required_caps = Some(caps),
            Err(_e) => {
                // Resolver failure is non-fatal: leave `required_caps` empty so
                // routing proceeds unconstrained rather than failing the
                // request closed on a transient store error.
            }
        }
    }

    // ---------------------------------------------------------------------
    // 7. Balance enforcement. Read the payer's available balance model:
    //     `base_micro - deducted_micro - frozen_micro`. Go owns base/deducted;
    //     Rust owns frozen via the async scalar INCRBY worker. The local DEDUCT
    //     snapshot is kept in sync so task/request freeze paths share the same
    //     in-process view.
    // ---------------------------------------------------------------------
    let balance = available_balance_micro_with_fallback(
        counters,
        overflow_gate,
        &merged.effective_user_id,
        !ctx.is_task && !ctx.is_gemini_files,
    )
    .await;
    sync_local_balance_snapshot(&merged.effective_user_id, balance);
    if balance.available <= 0 {
        let err = ErrorIR {
            category: ErrorCategory::ClientQuota,
            status: 402,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_QUOTA_BALANCE_INSUFFICIENT),
            message: Cow::Borrowed("user balance exhausted"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: Some(30),
        };
        bail!(session, ctx, &err, &request_id)
    }

    if ctx.is_gemini_files {
        let action = ctx.gemini_files_action.ok_or(ErrorIR {
            category: ErrorCategory::GatewayInternal,
            status: 500,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
            message: Cow::Borrowed("gemini files action missing"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        });
        let action = match action {
            Ok(action) => action,
            Err(err) => bail!(session, ctx, &err, &request_id),
        };
        let vendor_key = if let Some(deps) = routing_deps {
            match crate::filters::gemini_files::select_files_vendor_key(
                ctx,
                deps.store,
                deps.health,
                deps.in_flight,
                deps.strategies,
                deps.latency_tracker,
            )
            .await
            {
                Ok(vendor_key) => vendor_key,
                Err(err) => {
                    let error_ir = ErrorIR {
                        category: ErrorCategory::UpstreamError,
                        status: err.status,
                        upstream_status: None,
                        code: Cow::Borrowed(err.code),
                        message: err.message,
                        retryable: err.status >= 500,
                        upstream_raw: None,
                        request_id: "",
                        retry_after_secs: None,
                    };
                    bail!(session, ctx, &error_ir, &request_id)
                }
            }
        } else {
            crate::filters::gemini_files::skeleton_vendor_key()
        };
        let files_ctx = match crate::filters::gemini_files::build_files_ctx(ctx, vendor_key) {
            Ok(files_ctx) => files_ctx,
            Err(err) => {
                let error_ir = ErrorIR {
                    category: ErrorCategory::GatewayInternal,
                    status: err.status,
                    upstream_status: None,
                    code: Cow::Borrowed(err.code),
                    message: err.message,
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &error_ir, &request_id)
            }
        };
        let content_type = session
            .req_header()
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream");
        let file_id = crate::filters::gemini_files::file_id_from_path(&req_path);
        let response = crate::filters::gemini_files::handle_endpoint(
            files_gateway,
            action,
            &files_ctx,
            body_bytes.clone(),
            content_type,
            file_id,
        )
        .await;
        match response {
            Ok(response) => {
                crate::filters::gemini_files::write_response(session, response).await?;
                ctx.phase_end();
                return Ok(true);
            }
            Err(err) => {
                let error_ir = ErrorIR {
                    category: match err.status {
                        400 | 404 => ErrorCategory::ClientBadRequest,
                        _ => ErrorCategory::GatewayInternal,
                    },
                    status: err.status,
                    upstream_status: None,
                    code: Cow::Borrowed(err.code),
                    message: err.message,
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &error_ir, &request_id)
            }
        }
    }

    let mut openai_compatible_submit = if let (Some(key), Some(client_request)) = (
        openai_task_request_key.as_ref(),
        openai_task_client_request.clone(),
    ) {
        let runtime = match crate::filters::task::resolve_task_runtime(store, &key).await {
            Ok(runtime) => runtime,
            Err(err) => {
                let error_ir = task_error_ir(err);
                bail!(session, ctx, &error_ir, &request_id)
            }
        };
        let headers = reqwest_header_map_from_session(session);
        let query = session.req_header().uri.query().unwrap_or_default();
        let modes = match crate::filters::task::resolve_execution_modes(
            &runtime.profile,
            &req_method,
            &req_path,
            &headers,
            query,
            Some(&client_request),
        ) {
            Ok(modes) => modes,
            Err(err) => {
                let error_ir = task_error_ir(err);
                bail!(session, ctx, &error_ir, &request_id)
            }
        };
        Some((runtime, modes, client_request))
    } else {
        None
    };

    if ctx
        .task_endpoint_candidate
        .as_ref()
        .is_some_and(|candidate| candidate.method == "GET" && candidate.task_kind == "video")
    {
        let Some(contract) = task_contract else {
            let err = ErrorIR {
                category: ErrorCategory::GatewayInternal,
                status: 500,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_TASK_SUBMIT_FAILED),
                message: Cow::Borrowed("task contract unavailable"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        };
        let Some(video_id) = openai_video_id_from_path(&req_path) else {
            let err = ErrorIR {
                category: ErrorCategory::ClientBadRequest,
                status: 404,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_TASK_NOT_FOUND),
                message: Cow::Borrowed("task not found"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        };
        let task_result = if req_path.ends_with("/content") {
            crate::filters::task::load_openai_video_content_endpoint(
                contract,
                video_id,
                user_id.as_ref(),
            )
            .await
        } else {
            crate::filters::task::load_openai_video_endpoint(contract, video_id, user_id.as_ref())
                .await
        };
        match task_result {
            Ok(response) => {
                crate::filters::task::write_task_response(session, response).await?;
                ctx.phase_end();
                return Ok(true);
            }
            Err(err) => {
                let error_ir = task_error_ir(err);
                bail!(session, ctx, &error_ir, &request_id)
            }
        }
    }

    if ctx.is_task {
        let Some(contract) = task_contract else {
            let err = ErrorIR {
                category: ErrorCategory::GatewayInternal,
                status: 500,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_TASK_SUBMIT_FAILED),
                message: Cow::Borrowed("task contract unavailable"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        };
        let task_result = match ctx.task_action {
            Some(crate::filters::task::TaskAction::Submit) => {
                crate::filters::task::submit_endpoint(
                    contract,
                    crate::filters::task::SubmitTaskRequest {
                        body: ctx
                            .body
                            .as_ref()
                            .and_then(|v| v.decoded(32 * 1024 * 1024).ok())
                            .map(|b| b.as_ref())
                            .unwrap_or(body_bytes.as_ref()),
                        user_id: user_id.as_ref(),
                        api_key_id: api_key_id.as_ref().map(ApiKeyId::as_ref),
                        client_protocol: ctx
                            .client_shape
                            .map(|shape| shape.format_id())
                            .unwrap_or("unknown"),
                        client_codec: ctx.client_codec.as_ref(),
                        trace_id: ctx.trace_id.as_ref(),
                        request_id: ctx.request_id.as_ref(),
                        billing_config: task_billing_config,
                        available_balance_micro: Some(balance.available),
                    },
                )
                .await
            }
            Some(crate::filters::task::TaskAction::Load) => {
                let Some(task_id) = task_id_from_path(&req_path, false) else {
                    let err = ErrorIR {
                        category: ErrorCategory::ClientBadRequest,
                        status: 404,
                        upstream_status: None,
                        code: Cow::Borrowed(codes::GW_TASK_NOT_FOUND),
                        message: Cow::Borrowed("task not found"),
                        retryable: false,
                        upstream_raw: None,
                        request_id: "",
                        retry_after_secs: None,
                    };
                    bail!(session, ctx, &err, &request_id)
                };
                crate::filters::task::load_endpoint(contract, task_id).await
            }
            Some(crate::filters::task::TaskAction::Watch) => {
                let Some(task_id) = task_id_from_path(&req_path, true) else {
                    let err = ErrorIR {
                        category: ErrorCategory::ClientBadRequest,
                        status: 404,
                        upstream_status: None,
                        code: Cow::Borrowed(codes::GW_TASK_NOT_FOUND),
                        message: Cow::Borrowed("task not found"),
                        retryable: false,
                        upstream_raw: None,
                        request_id: "",
                        retry_after_secs: None,
                    };
                    bail!(session, ctx, &err, &request_id)
                };
                crate::filters::task::watch_endpoint(contract, task_id, task_wait_ms(session)).await
            }
            Some(crate::filters::task::TaskAction::Cancel) => {
                let Some(task_id) = task_id_from_path(&req_path, true) else {
                    let err = ErrorIR {
                        category: ErrorCategory::ClientBadRequest,
                        status: 404,
                        upstream_status: None,
                        code: Cow::Borrowed(codes::GW_TASK_NOT_FOUND),
                        message: Cow::Borrowed("task not found"),
                        retryable: false,
                        upstream_raw: None,
                        request_id: "",
                        retry_after_secs: None,
                    };
                    bail!(session, ctx, &err, &request_id)
                };
                crate::filters::task::cancel_endpoint(contract, task_id).await
            }
            None => {
                let err = ErrorIR {
                    category: ErrorCategory::GatewayInternal,
                    status: 500,
                    upstream_status: None,
                    code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
                    message: Cow::Borrowed("task action missing"),
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &err, &request_id)
            }
        };
        match task_result {
            Ok(response) => {
                crate::filters::task::write_task_response(session, response).await?;
                ctx.phase_end();
                return Ok(true);
            }
            Err(err) => {
                let category = match err.status {
                    402 => ErrorCategory::ClientQuota,
                    404 => ErrorCategory::ClientBadRequest,
                    _ => ErrorCategory::GatewayInternal,
                };
                let error_ir = ErrorIR {
                    category,
                    status: err.status,
                    upstream_status: None,
                    code: Cow::Borrowed(err.code),
                    message: err.message,
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: None,
                };
                bail!(session, ctx, &error_ir, &request_id)
            }
        }
    }

    // ---------------------------------------------------------------------
    // 7a. G3 — daily token quota (ADR-G3-1/G3-2). `UserEntry.daily_quota_tokens`
    //     is the limit; `0` means unlimited. We GET the per-(user, UTC-day)
    //     counter and pre-reject when `consumed + freeze_estimate` would
    //     exceed the quota. The freeze estimate is intentionally added so the
    //     gate is fail-closed against burst traffic that has not yet been
    //     accounted for by `logging`.
    //
    //     Redis failure fails open (consistent with the balance check above —
    //     daily quota is observational, not billing). Quota = 0 short-circuits
    //     before the Redis read. The post-increment lives in `logging`.
    // ---------------------------------------------------------------------
    let daily_quota = ctx
        .user
        .as_ref()
        .map(|u| u.value.daily_quota_tokens)
        .unwrap_or(0);
    if daily_quota > 0 {
        let day = day_key_utc(OffsetDateTime::now_utc());
        let key = daily_tokens_key(&merged.effective_user_id, &day);
        let consumed = counters.get_str_key(&key).await.unwrap_or(0).max(0) as u64;
        // Estimate tokens, not money. `freeze_estimate_with_pricing` returns
        // micro-units and must not be reused for a token quota.
        let quota_estimate_tokens = daily_quota_estimated_tokens(
            est_input_tokens,
            req_max_tokens.map(u64::from),
            estimate_pricing.as_ref().map(|p| p.value.as_ref()),
        );
        if check_daily_quota(consumed, quota_estimate_tokens, daily_quota).is_err() {
            let err = ErrorIR {
                category: ErrorCategory::ClientQuota,
                status: 403,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_QUOTA_DAILY_TOKENS_EXCEEDED),
                message: Cow::Borrowed("daily token quota exceeded"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        }
    }

    // ---------------------------------------------------------------------
    // 7b. USER_CIRCUIT breaker (zero-balance / banned-user short-circuit).
    //     Acts as an in-process cache: once a request observes a negative
    //     balance and the ledger flusher trips this circuit, subsequent
    //     requests within the open window skip the Redis read above.
    // ---------------------------------------------------------------------
    if let Some(circuit) = USER_CIRCUIT.get(&merged.effective_user_id) {
        if let UserCircuit::Open { until, .. } = *circuit.value() {
            if Instant::now() < until {
                let err = ErrorIR {
                    category: ErrorCategory::ClientQuota,
                    status: 402,
                    upstream_status: None,
                    code: Cow::Borrowed(codes::GW_QUOTA_BALANCE_INSUFFICIENT),
                    message: Cow::Borrowed("user circuit open"),
                    retryable: false,
                    upstream_raw: None,
                    request_id: "",
                    retry_after_secs: Some(30),
                };
                bail!(session, ctx, &err, &request_id)
            }
        }
    }

    // ---------------------------------------------------------------------
    // 8. RateLimitGuard (RPM check) + per-user stream-slot for streaming
    //    requests.
    // ---------------------------------------------------------------------
    // NOTE (audit 2026-05-30 §1.8): the per-user RPM bucket is enforced here.
    // The adaptive per-VENDOR-KEY ceiling is NOT applied at this point — the
    // vendor key is not picked until `pick_peer` (upstream_peer phase), so the
    // former clamp here was dead code (gated on ctx.picked_vendor_key, always
    // None in request_filter). Adaptive enforcement now lives in `pick_peer`
    // via AdaptiveRpmManager::try_admit, where the picked key is known.
    match ratelimit.check(&merged) {
        Ok(_ticket) => { /* dropped at end of fn — RPM only checks once per request */ }
        Err(err) => {
            bail!(session, ctx, &err, &request_id)
        }
    }
    if is_stream {
        match ratelimit.check_stream_slot(&merged.effective_user_id, merged.max_concurrent_streams)
        {
            Ok(slot) => ctx.stream_slot = Some(slot),
            Err(err) => {
                bail!(session, ctx, &err, &request_id)
            }
        }
    }

    // ---------------------------------------------------------------------
    // 8.5. Credit Lease (protected mode). Spec 2026-05-29-credit-lease-design.
    //      If the user is in protected mode (Go pushed `mode=protected` via
    //      `events:counter ns=protected_lease`), enforce a HARD upper bound on
    //      the per-Pod credits pool instead of the legacy frozen_micro soft
    //      defense. On success, bypasses step 9-10 entirely and returns; the
    //      LeaseGuard sits on ctx and is released in `logging`.
    // ---------------------------------------------------------------------
    if protection_cfg.enabled {
        let in_protected = gw_ledger::LEASE_POOL
            .get(&merged.effective_user_id)
            .map(|s| s.mode() == gw_ledger::Mode::Protected)
            .unwrap_or(false);

        if in_protected {
            let model_id_str = model_name.as_ref().to_string();
            let freeze_estimate = freeze_estimate_with_pricing(
                est_input_tokens,
                req_max_tokens.map(u64::from),
                merged.markup_chain.product,
                estimate_pricing
                    .as_ref()
                    .map(|p| (p.value.as_ref(), p.version)),
            );
            stash_prompt_estimate_ctx(ctx, est_input_tokens, freeze_estimate);

            // Resolve max_output_tokens with 3-tier fallback:
            //   1. request explicit max_tokens
            //   2. (no per-model default exists in MergedPermission yet — skip)
            //   3. configured ceiling
            let max_out = req_max_tokens.unwrap_or_else(|| {
                metrics::counter!(
                    "gw_credit_default_ceiling_used_total",
                    "model_id" => model_id_str.clone()
                )
                .increment(1);
                protection_cfg.default_output_ceiling_tokens
            });

            // Read the lease's coef_table_version (the version this lease was
            // priced under). settle MUST use the same version.
            let coef_version = gw_ledger::LEASE_POOL
                .get(&merged.effective_user_id)
                .map(|s| {
                    s.coef_table_version
                        .load(std::sync::atomic::Ordering::Acquire)
                })
                .unwrap_or(0);

            let credits = match gw_ledger::estimate_request_credits(
                coef_version,
                model_id_str.as_str(),
                est_input_tokens,
                max_out,
            ) {
                Ok(c) => c,
                Err(_) => {
                    // Coefs unavailable for this (version, model). Fail-closed:
                    // protected mode without coef data cannot safely admit
                    // requests.
                    metrics::counter!(
                        gw_ledger::metrics::CREDIT_LEASE_ACQUIRE_TOTAL,
                        "result" => "no_coefs"
                    )
                    .increment(1);
                    let err = ErrorIR {
                        category: ErrorCategory::ClientQuota,
                        status: 503,
                        upstream_status: None,
                        code: Cow::Borrowed(codes::GW_QUOTA_COEF_TABLE_NOT_READY),
                        message: Cow::Borrowed(
                            "credit coefficients unavailable for protected mode",
                        ),
                        retryable: false,
                        upstream_raw: None,
                        request_id: "",
                        retry_after_secs: None,
                    };
                    bail!(session, ctx, &err, &request_id)
                }
            };
            metrics::histogram!(
                gw_ledger::metrics::CREDIT_LEASE_ESTIMATE_CREDITS,
                "model_id" => model_id_str.clone()
            )
            .record(credits as f64);

            let now = gw_ledger::now_ms();
            match gw_ledger::LeaseGuard::try_acquire(
                &merged.effective_user_id,
                credits,
                now,
                protection_cfg.lease_grace_ms,
            ) {
                Ok(guard) => {
                    metrics::counter!(
                        gw_ledger::metrics::CREDIT_LEASE_ACQUIRE_TOTAL,
                        "result" => "success"
                    )
                    .increment(1);
                    ctx.lease_guard = Some(guard);
                    register_admitted_activity(
                        active_registration,
                        &merged.effective_user_id,
                        now,
                        protection_cfg.enabled,
                        ActiveRegistrationEndpoint::from_ctx(ctx),
                        ActiveRegistrationAdmission::LeaseSuccess,
                    );
                    if let Some((runtime, modes, client_request)) = openai_compatible_submit.take()
                    {
                        ctx.merged_perm = Some(merged);
                        execute_openai_compatible_submit_after_admission(
                            session,
                            ctx,
                            runtime,
                            modes,
                            client_request,
                            task_contract,
                            user_id.as_ref(),
                            api_key_id.as_ref(),
                            task_billing_config,
                            Some(balance.available),
                        )
                        .await?;
                        ctx.phase_end();
                        return Ok(true);
                    }
                    ctx.merged_perm = Some(merged);
                    ctx.phase_end();
                    return Ok(false);
                }
                Err(gw_ledger::LeaseError::Expired) => {
                    metrics::counter!(
                        gw_ledger::metrics::CREDIT_LEASE_ACQUIRE_TOTAL,
                        "result" => "expired"
                    )
                    .increment(1);
                    metrics::counter!("gw_credit_lease_expired_total").increment(1);
                    if let Err(err) = run_overflow_gate(
                        overflow_gate,
                        &merged.effective_user_id,
                        credits,
                        "expired",
                    )
                    .await
                    {
                        bail!(session, ctx, &err, &request_id)
                    }
                    {
                        register_admitted_activity(
                            active_registration,
                            &merged.effective_user_id,
                            now,
                            protection_cfg.enabled,
                            ActiveRegistrationEndpoint::from_ctx(ctx),
                            ActiveRegistrationAdmission::OverflowAdmittedProtectedFailure,
                        );
                        if let Some((runtime, modes, client_request)) =
                            openai_compatible_submit.take()
                        {
                            ctx.merged_perm = Some(merged);
                            execute_openai_compatible_submit_after_admission(
                                session,
                                ctx,
                                runtime,
                                modes,
                                client_request,
                                task_contract,
                                user_id.as_ref(),
                                api_key_id.as_ref(),
                                task_billing_config,
                                Some(balance.available),
                            )
                            .await?;
                            ctx.phase_end();
                            return Ok(true);
                        }
                        ctx.merged_perm = Some(merged);
                        ctx.phase_end();
                        return Ok(false);
                    }
                }
                Err(gw_ledger::LeaseError::Insufficient { .. }) => {
                    metrics::counter!(
                        gw_ledger::metrics::CREDIT_LEASE_ACQUIRE_TOTAL,
                        "result" => "insufficient"
                    )
                    .increment(1);
                    if let Err(err) = run_overflow_gate(
                        overflow_gate,
                        &merged.effective_user_id,
                        credits,
                        "insufficient",
                    )
                    .await
                    {
                        bail!(session, ctx, &err, &request_id)
                    }
                    {
                        register_admitted_activity(
                            active_registration,
                            &merged.effective_user_id,
                            now,
                            protection_cfg.enabled,
                            ActiveRegistrationEndpoint::from_ctx(ctx),
                            ActiveRegistrationAdmission::OverflowAdmittedProtectedFailure,
                        );
                        if let Some((runtime, modes, client_request)) =
                            openai_compatible_submit.take()
                        {
                            ctx.merged_perm = Some(merged);
                            execute_openai_compatible_submit_after_admission(
                                session,
                                ctx,
                                runtime,
                                modes,
                                client_request,
                                task_contract,
                                user_id.as_ref(),
                                api_key_id.as_ref(),
                                task_billing_config,
                                Some(balance.available),
                            )
                            .await?;
                            ctx.phase_end();
                            return Ok(true);
                        }
                        ctx.merged_perm = Some(merged);
                        ctx.phase_end();
                        return Ok(false);
                    }
                }
                Err(gw_ledger::LeaseError::NoLease) => {
                    // Race: in_protected was true at the get, but state was
                    // wiped between then and try_acquire. Hard mode routes
                    // this to overflow before any legacy soft-defense path.
                    metrics::counter!(
                        gw_ledger::metrics::CREDIT_LEASE_ACQUIRE_TOTAL,
                        "result" => "nolease"
                    )
                    .increment(1);
                    metrics::counter!("gw_credit_lease_warmup_pending_total").increment(1);
                    if let Err(err) = run_overflow_gate(
                        overflow_gate,
                        &merged.effective_user_id,
                        credits,
                        "no_lease",
                    )
                    .await
                    {
                        bail!(session, ctx, &err, &request_id)
                    }
                    {
                        register_admitted_activity(
                            active_registration,
                            &merged.effective_user_id,
                            now,
                            protection_cfg.enabled,
                            ActiveRegistrationEndpoint::from_ctx(ctx),
                            ActiveRegistrationAdmission::OverflowAdmittedProtectedFailure,
                        );
                        if let Some((runtime, modes, client_request)) =
                            openai_compatible_submit.take()
                        {
                            ctx.merged_perm = Some(merged);
                            execute_openai_compatible_submit_after_admission(
                                session,
                                ctx,
                                runtime,
                                modes,
                                client_request,
                                task_contract,
                                user_id.as_ref(),
                                api_key_id.as_ref(),
                                task_billing_config,
                                Some(balance.available),
                            )
                            .await?;
                            ctx.phase_end();
                            return Ok(true);
                        }
                        ctx.merged_perm = Some(merged);
                        ctx.phase_end();
                        return Ok(false);
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------------------
    // 9. Hard caps (single-request size + cost).
    // ---------------------------------------------------------------------
    use gw_auth::hard_cap::{check_cost, check_input_tokens};
    if let Err(err) = check_input_tokens(est_input_tokens, hard_cap_cfg) {
        bail!(session, ctx, &err, &request_id)
    }
    // freeze-estimate uses a flat 1 micro per estimated token times the
    // markup product. The freeze is released in `logging`; Go computes the
    // real cost from the UsageRecord.
    let freeze_estimate = freeze_estimate_with_pricing(
        est_input_tokens,
        req_max_tokens.map(u64::from),
        merged.markup_chain.product,
        estimate_pricing
            .as_ref()
            .map(|p| (p.value.as_ref(), p.version)),
    );
    let freeze_estimate_micro = freeze_estimate.amount_micro;
    stash_prompt_estimate_ctx(ctx, est_input_tokens, freeze_estimate);
    if let Err(err) = check_cost(freeze_estimate_micro, hard_cap_cfg) {
        bail!(session, ctx, &err, &request_id)
    }

    // ---------------------------------------------------------------------
    // 10. RequestFreeze. On drop without `release()` the destructor rolls
    //     back the frozen amount (leak-recovered metric increments).
    // ---------------------------------------------------------------------
    use gw_ledger::RequestFreeze;
    let guard = match RequestFreeze::new(merged.effective_user_id.clone(), freeze_estimate_micro) {
        Ok(guard) => guard,
        Err(err) => {
            let err = ErrorIR {
                category: ErrorCategory::ClientBadRequest,
                status: 400,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_AMOUNT_OVERFLOW),
                message: Cow::Owned(err.to_string()),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        }
    };
    ctx.freeze_guard = Some(guard);
    register_admitted_activity(
        active_registration,
        &merged.effective_user_id,
        gw_ledger::now_ms(),
        protection_cfg.enabled,
        ActiveRegistrationEndpoint::from_ctx(ctx),
        ActiveRegistrationAdmission::ColdFreezeAdmission,
    );

    if let Some((runtime, modes, client_request)) = openai_compatible_submit.take() {
        ctx.merged_perm = Some(merged);
        execute_openai_compatible_submit_after_admission(
            session,
            ctx,
            runtime,
            modes,
            client_request,
            task_contract,
            user_id.as_ref(),
            api_key_id.as_ref(),
            task_billing_config,
            Some(balance.available),
        )
        .await?;
        ctx.phase_end();
        return Ok(true);
    }

    ctx.merged_perm = Some(merged);
    ctx.phase_end();
    Ok(false)
}

/// G1 — closed-interval expiry check for an API key. `now` is injected so the
/// helper is testable without mocking the clock. `None` means never expires;
/// otherwise the key is valid only while `now < expires_at` (a request issued
/// exactly at the expiry instant rejects).
#[allow(clippy::result_unit_err)]
pub fn check_api_key_expiry(
    entry: &ApiKeyEntry,
    now: OffsetDateTime,
) -> std::result::Result<(), ()> {
    match entry.expires_at {
        None => Ok(()),
        Some(exp) if now < exp => Ok(()),
        _ => Err(()),
    }
}

/// Admit-time status gate for API keys (audit 2026-05-30). Pure so a test can
/// pin it. A key flipped to `Disabled` via `op=status_change` keeps its row
/// (`deleted_at` still NULL) and so loads normally — the loader only filters
/// `deleted_at`. Exhaustive `match` (not `matches!`) so a future `ApiKeyStatus`
/// variant forces a compile error here rather than silently admitting.
#[allow(clippy::result_unit_err)]
pub fn check_api_key_status(entry: &ApiKeyEntry) -> std::result::Result<(), ()> {
    match entry.status {
        gw_core::entity::ApiKeyStatus::Active => Ok(()),
        gw_core::entity::ApiKeyStatus::Disabled => Err(()),
    }
}

/// Resolved API-key auth scope: `(user id, api-key id, per-key IP allowlist)`.
/// Aliased so [`resolve_api_key_entry`]'s return type stays under clippy's
/// `type_complexity` threshold.
type ApiKeyAuthScope = (UserId, Option<ApiKeyId>, Option<Vec<String>>);

/// Shared post-auth gate for a resolved API-key entry, used by both the
/// single-secret shapes (`x-api-key` without id, `Authorization: Bearer ak_…`)
/// and the legacy `x-api-key` + `x-api-key-id` pair. Runs the disabled/expired
/// gates, stashes the HMAC signature requirement, and returns the scope tuple.
/// Pure (no `.await`): all DB resolution happens before this is called, so a
/// single helper keeps the two call sites byte-for-byte identical.
fn resolve_api_key_entry(
    session: &Session,
    ctx: &mut ReqCtx,
    entry: &Arc<Versioned<ApiKeyEntry>>,
    akid: ApiKeyId,
    signature_required: bool,
) -> std::result::Result<ApiKeyAuthScope, ErrorIR<'static>> {
    if check_api_key_status(&entry.value).is_err() {
        return Err(ErrorIR {
            category: ErrorCategory::ClientAuth,
            status: 401,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_AUTH_KEY_DISABLED),
            message: Cow::Borrowed("api key disabled"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        });
    }
    if check_api_key_expiry(&entry.value, OffsetDateTime::now_utc()).is_err() {
        return Err(ErrorIR {
            category: ErrorCategory::ClientAuth,
            status: 401,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_AUTH_KEY_EXPIRED),
            message: Cow::Borrowed("api key expired"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        });
    }
    if signature_required || entry.value.signature_required {
        ctx.signature_required = true;
        ctx.signature_algo = entry.value.signature_algo;
        ctx.signature_received = session
            .req_header()
            .headers
            .get("x-signature")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.as_bytes().to_vec());
        ctx.signature_timestamp = session
            .req_header()
            .headers
            .get("x-timestamp")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        ctx.signature_nonce = session
            .req_header()
            .headers
            .get("x-nonce")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        ctx.signature_secret = entry.value.signature_secret.clone().unwrap_or_default();
    }
    Ok((
        entry.value.user_id.clone(),
        Some(akid),
        entry.value.ip_allowlist.clone(),
    ))
}

/// Admit-time status gate for users (audit 2026-05-30). Pure so a test can pin
/// it. Same rationale as [`check_api_key_status`]: a `Banned` row loads normally.
#[allow(clippy::result_unit_err)]
pub fn check_user_status(entry: &gw_core::entity::UserEntry) -> std::result::Result<(), ()> {
    match entry.status {
        gw_core::entity::UserStatus::Active => Ok(()),
        gw_core::entity::UserStatus::Banned => Err(()),
    }
}

#[allow(clippy::result_unit_err)]
pub fn check_token_version(jwt_tv: Option<u64>, user_tv: u64) -> std::result::Result<(), ()> {
    match jwt_tv {
        Some(tv) if tv >= user_tv => Ok(()),
        None if user_tv == 0 => Ok(()),
        _ => Err(()),
    }
}

/// G2 — per-key IP allowlist gate. Pure (no async, no I/O) so a test can pin
/// both inputs. `None` and an empty allowlist mean unrestricted; otherwise
/// `client_ip` must match at least one CIDR in the list. Malformed entries
/// fail closed (skipped, never auto-match) inside `match_any_cidr`.
#[allow(clippy::result_unit_err)]
pub fn check_api_key_ip_allowlist(
    allowlist: Option<&[String]>,
    client_ip: IpAddr,
) -> std::result::Result<(), ()> {
    match allowlist {
        None => Ok(()),
        Some([]) => Ok(()),
        Some(list) if gw_auth::ip_filter::match_any_cidr(client_ip, list) => Ok(()),
        _ => Err(()),
    }
}

/// G3 (ADR-G3-2) — pure daily-token quota gate. `quota == 0` is treated as
/// "no quota" (unlimited). Otherwise reject when `consumed_today + freeze_est`
/// would exceed the configured quota. Saturating add so a pathological
/// estimate cannot wrap around silently.
#[allow(clippy::result_unit_err)]
pub fn check_daily_quota(
    consumed_today: u64,
    freeze_estimate: u64,
    quota: u64,
) -> std::result::Result<(), ()> {
    if quota == 0 {
        return Ok(());
    }
    if consumed_today.saturating_add(freeze_estimate) > quota {
        return Err(());
    }
    Ok(())
}

/// G3 — format an `OffsetDateTime` as a UTC calendar day key (`YYYY-MM-DD`).
/// Centralised so the request-filter pre-check and the logging post-increment
/// cannot disagree on day boundaries. UTC by construction; per-tenant timezone
/// support requires Go billing coordination; tracked separately.
pub fn day_key_utc(now: OffsetDateTime) -> String {
    let d = now.date();
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

fn task_id_from_path(path: &str, trailing_action: bool) -> Option<&str> {
    let segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    match (trailing_action, segments.as_slice()) {
        (false, ["v1", "tasks", task_id]) if !task_id.is_empty() => Some(task_id),
        (true, ["v1", "tasks", task_id, _]) if !task_id.is_empty() => Some(task_id),
        _ => None,
    }
}

fn openai_video_id_from_path(path: &str) -> Option<&str> {
    let segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    match segments.as_slice() {
        ["v1", "videos", video_id] if !video_id.is_empty() => Some(video_id),
        ["v1", "videos", video_id, "content"] if !video_id.is_empty() => Some(video_id),
        _ => None,
    }
}

fn task_request_requires_model_permission(ctx: &ReqCtx) -> bool {
    if !ctx.is_task {
        return true;
    }
    if ctx
        .task_endpoint_candidate
        .as_ref()
        .is_some_and(crate::filters::task::should_parse_openai_compatible_client_request)
    {
        return true;
    }
    matches!(
        ctx.task_action,
        Some(crate::filters::task::TaskAction::Submit)
    )
}

fn task_error_ir(err: crate::filters::task::TaskHttpError) -> ErrorIR<'static> {
    let category = match err.status {
        400 | 404 => ErrorCategory::ClientBadRequest,
        402 => ErrorCategory::ClientQuota,
        _ => ErrorCategory::GatewayInternal,
    };
    ErrorIR {
        category,
        status: err.status,
        upstream_status: None,
        code: Cow::Borrowed(err.code),
        message: err.message,
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_openai_compatible_submit_after_admission(
    session: &mut Session,
    ctx: &mut ReqCtx,
    runtime: crate::filters::task::ResolvedTaskRuntime,
    modes: crate::filters::task::ResolvedExecutionModes,
    client_request: serde_json::Value,
    task_contract: Option<&gw_task::redis_contract::RedisTaskContract>,
    user_id: &str,
    api_key_id: Option<&ApiKeyId>,
    task_billing_config: Option<TaskBillingConfig>,
    available_balance_micro: Option<i64>,
) -> Result<()> {
    let request_id = ctx.request_id.0.clone();
    let task_result = if modes.client_mode == gw_task::ClientMode::Async {
        let Some(contract) = task_contract else {
            let err = ErrorIR {
                category: ErrorCategory::GatewayInternal,
                status: 500,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_TASK_SUBMIT_FAILED),
                message: Cow::Borrowed("task contract unavailable"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            emit_and_bail(session, ctx, &err, &request_id).await?;
            unreachable!("emit_and_bail always returns Err; see fn doc")
        };
        crate::filters::task::submit_openai_compatible_task(
            contract,
            runtime,
            modes,
            client_request,
            user_id,
            api_key_id.map(ApiKeyId::as_ref),
            ctx.trace_id.as_ref(),
            ctx.request_id.as_ref(),
            task_billing_config,
            available_balance_micro,
        )
        .await
    } else {
        let upstream_base_url = runtime.upstream_base_url.clone();
        let transport =
            match gw_task_runtime::http_transport::HttpTemplateTransport::with_default_timeout(
                upstream_base_url,
            ) {
                Ok(transport) => Arc::new(transport),
                Err(err) => {
                    let error_ir = ErrorIR {
                        category: ErrorCategory::GatewayInternal,
                        status: 500,
                        upstream_status: None,
                        code: Cow::Borrowed(codes::GW_TASK_SUBMIT_FAILED),
                        message: Cow::Owned(format!("build task http transport: {err}")),
                        retryable: false,
                        upstream_raw: None,
                        request_id: "",
                        retry_after_secs: None,
                    };
                    emit_and_bail(session, ctx, &error_ir, &request_id).await?;
                    unreachable!("emit_and_bail always returns Err; see fn doc")
                }
            };
        crate::filters::task::handle_openai_compatible_submit(
            runtime,
            modes,
            client_request,
            transport,
            user_id,
            api_key_id.map(ApiKeyId::as_ref),
            ctx.trace_id.as_ref(),
            ctx.request_id.as_ref(),
        )
        .await
    };

    match task_result {
        Ok(response) => crate::filters::task::write_task_response(session, response).await,
        Err(err) => {
            let error_ir = task_error_ir(err);
            emit_and_bail(session, ctx, &error_ir, &request_id).await?;
            unreachable!("emit_and_bail always returns Err; see fn doc")
        }
    }
}

fn reqwest_header_map_from_session(session: &Session) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for (name, value) in &session.req_header().headers {
        let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()) else {
            continue;
        };
        let Ok(value) = reqwest::header::HeaderValue::from_bytes(value.as_bytes()) else {
            continue;
        };
        out.append(name, value);
    }
    out
}

fn task_wait_ms(session: &Session) -> u64 {
    session
        .req_header()
        .uri
        .query()
        .and_then(|query| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (key == "wait").then_some(value)
            })
        })
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1_000))
        .unwrap_or(30_000)
}

pub fn extract_cli_request_hints(
    shape: ApiShape,
    path: &str,
    body: Option<&[u8]>,
) -> gw_codec::cli::CliHints {
    match shape {
        ApiShape::ClaudeCli => gw_codec::cli::claude_cli::ClaudeCliCodec.extract_hints(path, body),
        ApiShape::OpenAiCli => gw_codec::cli::openai_cli::OpenAiCliCodec.extract_hints(path, body),
        ApiShape::OpenAiCompact => {
            gw_codec::cli::openai_compact::OpenAiCompactCodec.extract_hints(path, body)
        }
        ApiShape::GeminiCli => gw_codec::cli::gemini_cli::GeminiCliCodec.extract_hints(path, body),
        _ => gw_codec::cli::CliHints::default(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FreezeEstimate {
    pub amount_micro: u64,
    pub estimate_pricing_version: u64,
}

/// Estimate a conservative freeze (in micro-units). `EstimatePricing` is a
/// Rust-side admission estimate, not a per-token cost contract.
pub fn freeze_estimate_with_pricing(
    est_input_tokens: u32,
    request_max_tokens: Option<u64>,
    markup: f32,
    pricing: Option<(&EstimatePricing, u64)>,
) -> FreezeEstimate {
    if let Some((pricing, version)) = pricing {
        return FreezeEstimate {
            amount_micro: gw_ledger::estimate_pricing::compute_freeze_estimate(
                pricing,
                est_input_tokens as u64,
                request_max_tokens,
                normalize_markup(markup),
            )
            .max(1),
            estimate_pricing_version: version,
        };
    }
    FreezeEstimate {
        amount_micro: freeze_estimate(est_input_tokens, markup),
        estimate_pricing_version: 0,
    }
}

fn stash_prompt_estimate_ctx(
    ctx: &mut ReqCtx,
    est_input_tokens: u32,
    freeze_estimate: FreezeEstimate,
) {
    ctx.prompt_estimate_tokens = Some(est_input_tokens);
    ctx.estimate_pricing_version = freeze_estimate.estimate_pricing_version;
}

fn normalize_markup(markup: f32) -> f64 {
    let bounded = markup.max(1.0) as f64;
    (bounded * 1_000_000.0).round() / 1_000_000.0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AvailableBalance {
    base_micro: i64,
    deducted_micro: i64,
    frozen_micro: i64,
    available: i64,
}

async fn available_balance_micro(counters: &CounterStore, uid: &UserId) -> AvailableBalance {
    let id = uid.as_ref();
    let base_micro = counters.get("user", id, "base_micro").await.unwrap_or(0);
    let deducted_micro = counters
        .get("user", id, "deducted_micro")
        .await
        .unwrap_or(0);
    let frozen_micro = counters.get("user", id, "frozen_micro").await.unwrap_or(0);
    AvailableBalance {
        base_micro,
        deducted_micro,
        frozen_micro,
        available: base_micro
            .saturating_sub(deducted_micro)
            .saturating_sub(frozen_micro),
    }
}

async fn available_balance_micro_with_fallback(
    counters: &CounterStore,
    overflow_gate: &Arc<gw_ledger::overflow::OverflowGate>,
    uid: &UserId,
    allow_fallback: bool,
) -> AvailableBalance {
    let balance = available_balance_micro(counters, uid).await;
    if balance.available > 0 || !allow_fallback {
        return balance;
    }

    match overflow_gate.fallback_balance_micro(uid.as_ref()).await {
        Ok(fallback) => fallback_balance_snapshot(balance, fallback),
        Err(err) => {
            tracing::warn!(
                target: "gw_proxy::request",
                user_id = uid.as_ref(),
                error = ?err,
                "cold balance fallback failed"
            );
            metrics::counter!(
                gw_ledger::metrics::CREDIT_BALANCE_FALLBACK_TOTAL,
                "result" => "error",
                "path" => "error"
            )
            .increment(1);
            balance
        }
    }
}

fn fallback_balance_snapshot(mut balance: AvailableBalance, fallback: i64) -> AvailableBalance {
    if fallback <= 0 {
        return balance;
    }
    balance.base_micro = fallback;
    balance.deducted_micro = 0;
    balance.frozen_micro = 0;
    balance.available = fallback;
    balance
}

fn sync_local_balance_snapshot(uid: &UserId, balance: AvailableBalance) {
    use std::sync::atomic::{AtomicI64, Ordering};
    let state = DEDUCT
        .entry(uid.clone())
        .or_insert_with(|| {
            Arc::new(DeductionState {
                base_micro: AtomicI64::new(0),
                deducted_micro: AtomicI64::new(0),
                frozen_micro: AtomicI64::new(0),
            })
        })
        .clone();
    state
        .base_micro
        .store(balance.base_micro, Ordering::Release);
    state
        .deducted_micro
        .store(balance.deducted_micro, Ordering::Release);
    state
        .frozen_micro
        .store(balance.frozen_micro, Ordering::Release);
}

fn daily_quota_estimated_tokens(
    est_input_tokens: u32,
    request_max_tokens: Option<u64>,
    pricing: Option<&EstimatePricing>,
) -> u64 {
    let input_tokens = est_input_tokens as u64;
    let Some(pricing) = pricing else {
        return input_tokens;
    };
    let max_output_by_hard_cap = (pricing.hard_cap_tokens as u64).saturating_sub(input_tokens);
    let output_cap = request_max_tokens
        .unwrap_or(pricing.default_max_tokens as u64)
        .min(max_output_by_hard_cap);
    input_tokens.saturating_add(output_cap)
}

/// Legacy fallback estimate before `entity:estimate_pricing` is available.
/// We use a flat 1 micro per estimated input token times the markup product.
fn freeze_estimate(est_input_tokens: u32, markup: f32) -> u64 {
    let base = est_input_tokens as u64;
    let with_markup = (base as f32 * markup.max(1.0)) as u64;
    with_markup.max(1)
}

fn model_regex_redos_tracker() -> &'static RedosTracker {
    static TRACKER: OnceLock<RedosTracker> = OnceLock::new();
    TRACKER.get_or_init(RedosTracker::new)
}

fn check_model_whitelist_regex(
    model_name: &ModelName,
    pattern: Option<&str>,
    api_key_id: Option<&str>,
    redos_tracker: &RedosTracker,
    regex_cache: &gw_runtime::RegexCache,
) -> std::result::Result<(), ErrorIR<'static>> {
    let Some(pattern) = pattern else {
        return Ok(());
    };
    let permission = match ModelPermission::new(regex_cache, pattern, PermissionEffect::Allow) {
        Ok(permission) => permission,
        Err(err) => {
            tracing::warn!(
                target: "gw_proxy::request_filter",
                error = %err,
                pattern,
                "model_whitelist_regex compile failed; denying request"
            );
            return Err(model_whitelist_regex_denied_error(
                "model_whitelist_regex compile failed",
            ));
        }
    };
    let api_key_id = api_key_id.unwrap_or("");
    match check_with_budget(
        model_name.as_ref(),
        &[permission],
        api_key_id,
        redos_tracker,
    ) {
        Some(true) => Ok(()),
        Some(false) => Err(model_whitelist_regex_denied_error(
            "model not in model_whitelist_regex",
        )),
        None => Err(model_whitelist_regex_denied_error(
            "model_whitelist_regex disabled for api key",
        )),
    }
}

fn model_whitelist_regex_denied_error(message: &'static str) -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientAuth,
        status: 403,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_REQUEST_MODEL_NOT_FOUND),
        message: Cow::Borrowed(message),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}

fn single_api_key_secret<'a>(
    bearer_token: Option<&'a str>,
    api_key_hdr: Option<&'a str>,
    api_key_id_hdr: Option<&str>,
) -> Option<&'a str> {
    api_key_hdr
        .filter(|_| api_key_id_hdr.is_none())
        .or_else(|| bearer_token.filter(|token| token.starts_with("ak_")))
}

pub fn apikey_module_blocks(api_key_id: Option<&ApiKeyId>, enabled: bool) -> bool {
    api_key_id.is_some() && !enabled
}

fn overflow_error_for_decision(
    decision: gw_ledger::overflow::OverflowDecision,
) -> Option<ErrorIR<'static>> {
    match decision {
        gw_ledger::overflow::OverflowDecision::Allow => None,
        gw_ledger::overflow::OverflowDecision::BalanceInsufficient => Some(ErrorIR {
            category: ErrorCategory::ClientQuota,
            status: 402,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_QUOTA_BALANCE_INSUFFICIENT),
            message: Cow::Borrowed("insufficient credits for this request"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        }),
    }
}

fn overflow_error_for_failure(err: &gw_ledger::overflow::OverflowError) -> ErrorIR<'static> {
    let message = match err {
        gw_ledger::overflow::OverflowError::CacheUnavailable => "balance cache unavailable",
        gw_ledger::overflow::OverflowError::Redis(_) => "balance cache unavailable",
        gw_ledger::overflow::OverflowError::Pg(_) => "balance fallback unavailable",
    };
    ErrorIR {
        category: ErrorCategory::UpstreamError,
        status: 503,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_BALANCE_CACHE_UNAVAILABLE),
        message: Cow::Borrowed(message),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}

async fn run_overflow_gate(
    overflow_gate: &Arc<gw_ledger::overflow::OverflowGate>,
    user_id: &UserId,
    estimate_credits: i64,
    lease_result: &'static str,
) -> std::result::Result<(), ErrorIR<'static>> {
    match overflow_gate
        .check(user_id.as_ref(), estimate_credits)
        .await
    {
        Ok(decision) => {
            if let Some(err) = overflow_error_for_decision(decision) {
                return Err(err);
            }
            Ok(())
        }
        Err(err) => {
            tracing::error!(
                target: "gw_proxy::request",
                user_id = %user_id,
                lease_result,
                error = ?err,
                "overflow gate failed"
            );
            Err(overflow_error_for_failure(&err))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveRegistrationEndpoint {
    NormalBillable,
    NonNormalEndpoint,
}

impl ActiveRegistrationEndpoint {
    fn from_ctx(ctx: &ReqCtx) -> Self {
        if ctx.is_task || ctx.is_gemini_files {
            Self::NonNormalEndpoint
        } else {
            Self::NormalBillable
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveRegistrationAdmission {
    LeaseSuccess,
    ColdFreezeAdmission,
    OverflowAdmittedProtectedFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveRegistrationDecision {
    Register,
    Skip,
}

fn active_registration_decision(
    protection_enabled: bool,
    endpoint: ActiveRegistrationEndpoint,
    admission: Option<ActiveRegistrationAdmission>,
) -> ActiveRegistrationDecision {
    if !protection_enabled || endpoint != ActiveRegistrationEndpoint::NormalBillable {
        return ActiveRegistrationDecision::Skip;
    }

    match admission {
        Some(
            ActiveRegistrationAdmission::LeaseSuccess
            | ActiveRegistrationAdmission::ColdFreezeAdmission
            | ActiveRegistrationAdmission::OverflowAdmittedProtectedFailure,
        ) => ActiveRegistrationDecision::Register,
        None => ActiveRegistrationDecision::Skip,
    }
}

fn register_admitted_activity(
    active_registration: &gw_ledger::ActiveRegistrationHandle,
    uid: &UserId,
    admitted_at_ms: u64,
    protection_enabled: bool,
    endpoint: ActiveRegistrationEndpoint,
    admission: ActiveRegistrationAdmission,
) {
    if active_registration_decision(protection_enabled, endpoint, Some(admission))
        != ActiveRegistrationDecision::Register
    {
        return;
    }

    let result = active_registration.try_enqueue_activity(uid, admitted_at_ms);
    tracing::debug!(
        target: "gw_proxy::request",
        user_id = uid.as_ref(),
        ?result,
        "active registration enqueue result"
    );
}

/// Emit an error via the client codec and return a short-circuit `Err` so the
/// `?` operator unwinds. Callers use the `bail!` macro, which appends the
/// `unreachable!()` in a single place rather than at every call site.
async fn emit_and_bail(
    session: &mut Session,
    ctx: &mut ReqCtx,
    err: &ErrorIR<'static>,
    request_id: &str,
) -> Result<()> {
    let codec = pick_emit_codec(&ctx.client_codec);
    let scoped = with_request_id(err, request_id);
    error_emit::send_error(session, &codec, &scoped).await;
    ctx.phase_end();
    Err(short_circuit(err.status))
}

#[cfg(test)]
mod api_key_auth_shape_tests {
    use super::*;

    #[test]
    fn single_x_api_key_is_accepted_without_key_id() {
        assert_eq!(
            single_api_key_secret(None, Some("ak_plain"), None),
            Some("ak_plain")
        );
    }

    #[test]
    fn paired_x_api_key_keeps_legacy_pair_path() {
        assert_eq!(
            single_api_key_secret(None, Some("ak_plain"), Some("ak_id")),
            None
        );
    }

    #[test]
    fn bearer_ak_token_is_treated_as_api_key() {
        assert_eq!(
            single_api_key_secret(Some("ak_plain"), None, None),
            Some("ak_plain")
        );
    }

    #[test]
    fn bearer_jwt_is_not_treated_as_api_key() {
        assert_eq!(
            single_api_key_secret(Some("eyJhbGciOiJIUzI1NiJ9.payload.sig"), None, None),
            None
        );
    }

    #[test]
    fn api_key_module_toggle_only_blocks_api_key_auth() {
        let akid = ApiKeyId::from("ak_test");

        assert!(apikey_module_blocks(Some(&akid), false));
        assert!(!apikey_module_blocks(Some(&akid), true));
        assert!(!apikey_module_blocks(None, false));
        assert!(!apikey_module_blocks(None, true));
    }
}

#[cfg(test)]
mod lease_expired_tests {
    use super::*;
    use gw_core::ids::ModelName;

    #[test]
    fn overflow_decision_error_mapping_keeps_quota_vs_system_codes_distinct() {
        assert!(
            overflow_error_for_decision(gw_ledger::overflow::OverflowDecision::Allow).is_none()
        );

        let insufficient =
            overflow_error_for_decision(gw_ledger::overflow::OverflowDecision::BalanceInsufficient)
                .expect("insufficient maps to error");
        assert_eq!(insufficient.status, 402);
        assert_eq!(
            insufficient.code,
            Cow::Borrowed(codes::GW_QUOTA_BALANCE_INSUFFICIENT)
        );
        assert_eq!(insufficient.category, ErrorCategory::ClientQuota);
    }

    #[test]
    fn overflow_failure_maps_to_503_cache_unavailable() {
        let err = overflow_error_for_failure(&gw_ledger::overflow::OverflowError::CacheUnavailable);
        assert_eq!(err.status, 503);
        assert_eq!(
            err.code,
            Cow::Borrowed(codes::GW_QUOTA_BALANCE_CACHE_UNAVAILABLE)
        );
        assert_eq!(err.category, ErrorCategory::UpstreamError);
    }

    #[test]
    fn daily_quota_estimate_counts_tokens_not_micro_units() {
        let pricing = EstimatePricing {
            model: ModelName::from("gpt-4o"),
            conservative_per_token_micro: 60,
            safety_factor: 2.0,
            default_max_tokens: 100,
            hard_cap_tokens: 1_000,
        };

        assert_eq!(
            daily_quota_estimated_tokens(1_000, Some(500), Some(&pricing)),
            1_000
        );
        assert_eq!(
            freeze_estimate_with_pricing(1_000, Some(500), 1.0, Some((&pricing, 9))).amount_micro,
            120_000
        );
    }

    #[test]
    fn daily_quota_estimate_uses_default_output_cap() {
        let pricing = EstimatePricing {
            model: ModelName::from("gpt-4o"),
            conservative_per_token_micro: 10,
            safety_factor: 1.0,
            default_max_tokens: 100,
            hard_cap_tokens: 150,
        };

        assert_eq!(daily_quota_estimated_tokens(20, None, Some(&pricing)), 120);
        assert_eq!(daily_quota_estimated_tokens(20, None, None), 20);
    }

    #[test]
    fn protected_cli_request_stashes_prompt_estimate_before_lease_return() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/filters/request.rs"
        ))
        .expect("read source");
        let protected_guard = src
            .find("ctx.lease_guard = Some(guard);")
            .expect("protected lease success path");
        let estimate_store = src
            .find("stash_prompt_estimate_ctx(ctx, est_input_tokens, freeze_estimate);")
            .expect("prompt estimate must be stashed");

        assert!(
            estimate_store < protected_guard,
            "protected CLI path must preserve prompt estimate before early lease return"
        );
    }

    #[test]
    fn stash_prompt_estimate_ctx_preserves_estimate_for_protected_settlement() {
        let mut ctx = ReqCtx::default();

        stash_prompt_estimate_ctx(
            &mut ctx,
            41,
            FreezeEstimate {
                amount_micro: 123,
                estimate_pricing_version: 9,
            },
        );

        assert_eq!(ctx.prompt_estimate_tokens, Some(41));
        assert_eq!(ctx.estimate_pricing_version, 9);
    }
}

#[cfg(test)]
mod non_stream_unit {
    /// B1-2: non-stream short-circuit must be deleted from request_filter.
    /// Pingora Session is awkward to mock at unit-test level, so we assert
    /// the absence of the guard via source-grep. End-to-end coverage lives
    /// in T4-A docker e2e (test_non_stream.py).
    #[test]
    fn non_stream_guard_removed_from_source() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/filters/request.rs"
        ))
        .expect("read source");
        // Build the needle at runtime so the literal in this test does not
        // self-match when the source file is read.
        let needle: String = ["GW_REQUEST_", "NONSTREAM_", "NOT_SUPPORTED"].concat();
        assert!(
            !src.contains(&needle),
            "non-stream short-circuit must be removed in B1-2"
        );
    }
}

#[cfg(test)]
mod active_registration_boundary_tests {
    use super::*;

    #[test]
    fn active_registration_is_emitted_for_admitted_normal_paths() {
        for admission in [
            ActiveRegistrationAdmission::LeaseSuccess,
            ActiveRegistrationAdmission::ColdFreezeAdmission,
            ActiveRegistrationAdmission::OverflowAdmittedProtectedFailure,
        ] {
            assert_eq!(
                active_registration_decision(
                    true,
                    ActiveRegistrationEndpoint::NormalBillable,
                    Some(admission)
                ),
                ActiveRegistrationDecision::Register,
                "{admission:?} should register"
            );
        }
    }

    #[test]
    fn active_registration_is_skipped_for_rejections_non_normal_and_disabled_protection() {
        assert_eq!(
            active_registration_decision(true, ActiveRegistrationEndpoint::NormalBillable, None,),
            ActiveRegistrationDecision::Skip
        );
        assert_eq!(
            active_registration_decision(
                true,
                ActiveRegistrationEndpoint::NonNormalEndpoint,
                Some(ActiveRegistrationAdmission::LeaseSuccess),
            ),
            ActiveRegistrationDecision::Skip
        );
        assert_eq!(
            active_registration_decision(
                false,
                ActiveRegistrationEndpoint::NormalBillable,
                Some(ActiveRegistrationAdmission::ColdFreezeAdmission),
            ),
            ActiveRegistrationDecision::Skip
        );
    }
}

#[cfg(test)]
mod available_balance_fallback_tests {
    use super::*;

    #[test]
    fn positive_authoritative_fallback_overrides_stale_local_counters() {
        let stale_local = AvailableBalance {
            base_micro: 0,
            deducted_micro: 70,
            frozen_micro: 50,
            available: -120,
        };

        let snapshot = fallback_balance_snapshot(stale_local, 100);

        assert_eq!(
            snapshot,
            AvailableBalance {
                base_micro: 100,
                deducted_micro: 0,
                frozen_micro: 0,
                available: 100,
            }
        );
        assert!(snapshot.available > 0);
    }

    #[test]
    fn zero_fallback_preserves_rejection_snapshot() {
        let stale_local = AvailableBalance {
            base_micro: 0,
            deducted_micro: 70,
            frozen_micro: 50,
            available: -120,
        };

        let snapshot = fallback_balance_snapshot(stale_local, 0);

        assert_eq!(snapshot, stale_local);
        assert!(snapshot.available <= 0);
    }
}

#[cfg(test)]
mod mgmt_path_branch_tests {
    /// B2-2 (T3-B) source-grep regression. Pingora Session is non-mockable
    /// here; end-to-end coverage is in T4-B docker e2e. We at least guard
    /// against the verify chain being silently deleted.
    #[test]
    fn mgmt_branch_calls_verify_helper() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/filters/request.rs"
        ))
        .expect("read source");
        // Runtime-built needles so this test file's literal doesn't self-match.
        let fn_needle: String = ["verify_mgmt_token", "_secret"].concat();
        let ok_needle: String = ["MgmtVerifyOutcome", "::", "Ok"].concat();
        let check_mgmt: String = ["check_", "mgmt"].concat();
        let load: String = ["mgmt_token_or", "_load"].concat();
        assert!(
            src.contains(&fn_needle),
            "T3-B helper integration missing — verify_mgmt_token_secret not called"
        );
        assert!(
            src.contains(&ok_needle),
            "T3-B outcome handling missing — MgmtVerifyOutcome::Ok arm absent"
        );
        assert!(
            src.contains(&check_mgmt),
            "T3-B blacklist integration missing — check_mgmt not called"
        );
        assert!(
            src.contains(&load),
            "T3-B store integration missing — mgmt_token_or_load not called"
        );
    }
}

#[cfg(test)]
mod model_whitelist_regex_tests {
    use super::*;
    use gw_auth::model_permissions::RedosTracker;

    #[test]
    fn regex_allows_matching_model() {
        let tracker = RedosTracker::new();
        let result = check_model_whitelist_regex(
            &ModelName::from("gpt-4o"),
            Some("^gpt-4.*"),
            Some("ak_regex_allow"),
            &tracker,
            &gw_runtime::RegexCache::new(gw_runtime::RegexCacheConfig::default()),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn regex_denies_non_matching_model_with_model_not_found_code() {
        let tracker = RedosTracker::new();
        let err = check_model_whitelist_regex(
            &ModelName::from("claude-3-5-sonnet"),
            Some("^gpt-4.*"),
            Some("ak_regex_deny"),
            &tracker,
            &gw_runtime::RegexCache::new(gw_runtime::RegexCacheConfig::default()),
        )
        .expect_err("non-matching model must be denied");

        assert_eq!(err.status, 403);
        assert_eq!(err.code, codes::GW_REQUEST_MODEL_NOT_FOUND);
    }

    #[test]
    fn regex_auto_disabled_tracker_denies_fail_closed() {
        let tracker = RedosTracker::new();
        let api_key_id = "ak_regex_disabled";
        for _ in 0..10 {
            tracker.record_timeout(api_key_id);
        }

        let err = check_model_whitelist_regex(
            &ModelName::from("gpt-4o"),
            Some("^gpt-4.*"),
            Some(api_key_id),
            &tracker,
            &gw_runtime::RegexCache::new(gw_runtime::RegexCacheConfig::default()),
        )
        .expect_err("disabled api key must fail closed");

        assert_eq!(err.status, 403);
        assert_eq!(err.code, codes::GW_REQUEST_MODEL_NOT_FOUND);
    }

    #[test]
    fn none_regex_skips_check_even_when_tracker_disabled() {
        let tracker = RedosTracker::new();
        let api_key_id = "ak_regex_skipped";
        for _ in 0..10 {
            tracker.record_timeout(api_key_id);
        }

        let result = check_model_whitelist_regex(
            &ModelName::from("any-model"),
            None,
            Some(api_key_id),
            &tracker,
            &gw_runtime::RegexCache::new(gw_runtime::RegexCacheConfig::default()),
        );

        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod token_version_gate_tests {
    use super::check_token_version;

    #[test]
    fn current_token_version_is_accepted() {
        assert!(check_token_version(Some(2), 2).is_ok());
    }

    #[test]
    fn newer_token_version_is_accepted() {
        assert!(check_token_version(Some(3), 2).is_ok());
    }

    #[test]
    fn stale_token_version_is_rejected() {
        assert!(check_token_version(Some(1), 2).is_err());
    }

    #[test]
    fn missing_tv_accepted_when_user_never_rotated() {
        assert!(check_token_version(None, 0).is_ok());
    }

    #[test]
    fn missing_tv_rejected_after_user_rotated() {
        assert!(check_token_version(None, 1).is_err());
    }
}
