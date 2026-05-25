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
use gw_auth::apikey::verify_apikey;
use gw_auth::ip_filter::check_blocklist;
use gw_auth::jwt::{verify_jwt, JwtError, JwtValidator};
use gw_auth::jwt_blacklist::RedisJwtBlacklist;
use gw_auth::merge_permissions;
use gw_auth::ratelimit::RateLimitGuard;
use gw_auth::AuthCtx;
use gw_core::entity::ApiKeyEntry;
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{ApiKeyId, ModelName, UserId};
use gw_store::counter_store::daily_tokens_key;
use gw_store::deduction::{UserCircuit, USER_CIRCUIT};
use gw_store::{CounterStore, EntityStore};
use pingora::Result;
use pingora_proxy::Session;
use std::borrow::Cow;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;
use time::OffsetDateTime;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    ratelimit: &Arc<RateLimitGuard>,
    counters: &Arc<CounterStore>,
    global_rpm: u32,
    trust_xff: bool,
    signature_required: bool,
    jwt_validator: Option<&Arc<JwtValidator>>,
    capability_resolver: Option<&Arc<gw_capability::CapabilityResolver>>,
    mgmt_blacklist: &Arc<RedisJwtBlacklist>,
    hard_cap_cfg: &gw_auth::hard_cap::HardCapCfg,
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
            ($status:expr, $code:expr) => {{
                let err = ErrorIR {
                    category: ErrorCategory::ClientAuth,
                    status: $status,
                    upstream_status: None,
                    code: Cow::Borrowed($code),
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
            mgmt_bail!(401, codes::GW_AUTH_MGMT_TOKEN_MISSING);
        };

        // 2. Split mt_<id>.<secret>
        let Some(parts) = gw_auth::mgmt::split_mgmt_token_payload(&parsed_auth.token) else {
            mgmt_bail!(401, codes::GW_AUTH_MGMT_TOKEN_INVALID);
        };

        // 3. Load entry via Moka L1 → cold load. Miss → INVALID (same code as
        //    hash mismatch, by design — see spec §1.3 step 4).
        let Some(entry) = store.mgmt_token_or_load(parts.token_id).await else {
            mgmt_bail!(401, codes::GW_AUTH_MGMT_TOKEN_INVALID);
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
            mgmt_bail!(401, codes::GW_AUTH_MGMT_TOKEN_REVOKED);
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
                mgmt_bail!(401, codes::GW_AUTH_MGMT_TOKEN_INVALID);
            }
            gw_auth::mgmt::MgmtVerifyOutcome::Expired => {
                mgmt_bail!(401, codes::GW_AUTH_MGMT_TOKEN_EXPIRED);
            }
            gw_auth::mgmt::MgmtVerifyOutcome::Disabled => {
                mgmt_bail!(401, codes::GW_AUTH_MGMT_TOKEN_DISABLED);
            }
            gw_auth::mgmt::MgmtVerifyOutcome::ScopeDenied => {
                mgmt_bail!(403, codes::GW_AUTH_MGMT_TOKEN_SCOPE_DENIED);
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

    let user_id_res: Result<(UserId, Option<ApiKeyId>, Option<Vec<String>>)> = if let Some(bearer) =
        auth_header
            .as_deref()
            .and_then(|h| h.strip_prefix("Bearer "))
    {
        // G6 (T1-E): if AuthConfig.global is wired into the gateway, use the
        // hybrid validator (JWKS resolver + iss/aud/nbf/leeway enforcement
        // with HS256 / static_pem_rs256 fallbacks). Otherwise fall through to
        // the legacy `verify_jwt` so deployments without AuthConfig keep
        // working with v1 semantics (exp-only, JWKS-by-kid).
        match jwt_validator {
            Some(v) => match v.validate_sub(bearer).await {
                Ok(uid) => Ok((uid, None, None)),
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
                Ok(uid) => Ok((uid, None, None)),
                Err(err) => {
                    bail!(session, ctx, &err, &request_id)
                }
            },
        }
    } else if let (Some(akid_str), Some(akey)) = (api_key_id_hdr.as_ref(), api_key_hdr.as_ref()) {
        let akid = ApiKeyId::from(akid_str.as_str());
        match verify_apikey(akey, &akid, store).await {
            Ok(()) => {
                // Resolve the user_id via the api-key entry.
                match store.apikey_or_load(&akid).await {
                    Some(entry) => {
                        // G1 — reject expired keys before any IR parse or
                        // permission merge. Closed interval: a request issued
                        // exactly at `expires_at` rejects (spec ADR-G1-1).
                        if check_api_key_expiry(&entry.value, OffsetDateTime::now_utc()).is_err() {
                            let err = ErrorIR {
                                category: ErrorCategory::ClientAuth,
                                status: 401,
                                upstream_status: None,
                                code: Cow::Borrowed(codes::GW_AUTH_KEY_EXPIRED),
                                message: Cow::Borrowed("api key expired"),
                                retryable: false,
                                upstream_raw: None,
                                request_id: "",
                                retry_after_secs: None,
                            };
                            bail!(session, ctx, &err, &request_id)
                        }
                        // T6-C: stash signature requirement state so the
                        // request_body_filter (or `body_verify::verify_body`)
                        // can run body-after HMAC verification.
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
                            ctx.signature_secret =
                                entry.value.signature_secret.clone().unwrap_or_default();
                        }
                        // G2 — capture the per-key IP allowlist so the
                        // post-blocklist gate (below) can consult it after
                        // `entry` itself goes out of scope.
                        let ip_allowlist_for_request = entry.value.ip_allowlist.clone();
                        Ok((
                            entry.value.user_id.clone(),
                            Some(akid),
                            ip_allowlist_for_request,
                        ))
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

    // Load the user entry (used by ReqCtx for downstream consumers).
    let user_entry = match store.user_or_load(&user_id).await {
        Some(u) => u,
        None => {
            let err = ErrorIR {
                category: ErrorCategory::ClientAuth,
                status: 401,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_AUTH_INVALID_APIKEY),
                message: Cow::Borrowed("user not found"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            bail!(session, ctx, &err, &request_id)
        }
    };
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
    // 2. Re-run IP blocklist with the resolved scope IDs.
    // ---------------------------------------------------------------------
    let client_ip_v = client_ip_from_session(session, trust_xff);
    if let Some(ip) = client_ip_v {
        if let Err(err) = check_blocklist(ip, Some(&user_id), api_key_id.as_ref(), store).await {
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
    ctx.cached_body = Some(body_bytes.clone());

    // Wire-fidelity archive: snapshot the raw downstream request. `cached_body`
    // is overwritten by `upstream::rewrite_request` on the cross-protocol path,
    // so the snapshot must happen now while we still have the original bytes.
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
            Ok((SignatureCheckResult::Valid, decoded))
            | Ok((SignatureCheckResult::NotRequired, decoded)) => {
                if !matches!(
                    ctx.req_content_encoding,
                    gw_codec::compression::Encoding::Identity
                ) {
                    ctx.cached_body = Some(decoded.clone());
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
    let codec = match ctx.client_codec.as_ref() {
        Some(c) => c,
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
    let parse_body = ctx
        .cached_body
        .as_ref()
        .cloned()
        .unwrap_or_else(|| body_bytes.clone());
    let parsed = codec.parse_request(&parse_body);
    let (model_name, is_stream, est_input_tokens) = match parsed {
        Ok(ir) => {
            let values = (
                ModelName::from(ir.model.as_ref()),
                ir.stream,
                codec.estimate_input_tokens(&ir),
            );
            values
        }
        Err(err) => {
            bail!(session, ctx, &err, &request_id)
        }
    };
    // ADR-S1: some wire formats (Gemini) signal streaming via the URL path
    // (`:streamGenerateContent`) rather than a body field. OR the codec's
    // path-aware hook into `is_stream` so those requests are not rejected by
    // the v1 non-stream gate below.
    let path_str = session.req_header().uri.path();
    let is_stream = is_stream || codec.path_implies_stream(path_str);
    // ADR-S1b: Gemini carries the model in the URL path (`/v1[beta]/models/<M>:<verb>`)
    // and `GeminiRequest::model` is `#[serde(skip)]`. Fall back to the
    // codec's path extractor when the body parse returned an empty model.
    let model_name = if model_name.as_ref().is_empty() {
        codec
            .extract_path_model(path_str)
            .map(ModelName::from)
            .unwrap_or(model_name)
    } else {
        model_name
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
    if !merged.allowed_models.is_empty() && !merged.allowed_models.contains(&model_name) {
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
    // 7. Balance enforcement. Read the payer's `balance_micro` counter from
    //     Redis and reject when it has fallen to zero or below. This is the
    //     primary zero-balance gate; the USER_CIRCUIT check below is a
    //     fast-path short-circuit that catches users whose circuit was tripped
    //     open in a prior request (e.g. by the ledger flusher on a debit
    //     that drove balance negative).
    //
    //     The Redis read is on the hot path but the read is a single GET
    //     against the connection pool — at typical Redis RTTs (<1ms LAN)
    //     this is well within the gateway's per-request budget. On Redis
    //     failure we choose to admit (fail-open) so a transient Redis blip
    //     does not block all paying users; the freeze guard still bounds
    //     loss to the freeze estimate.
    // ---------------------------------------------------------------------
    let balance = counters
        .get("user", merged.effective_user_id.as_ref(), "balance_micro")
        .await
        .unwrap_or(0);
    if balance <= 0 {
        let err = ErrorIR {
            category: ErrorCategory::ClientQuota,
            status: 402,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_QUOTA_BALANCE_ZERO),
            message: Cow::Borrowed("user balance exhausted"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: Some(30),
        };
        bail!(session, ctx, &err, &request_id)
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
        // freeze_estimate is computed at step 9; pre-compute a conservative
        // upper bound here using the same flat formula so the gate is closed
        // before USER_CIRCUIT and RPM consume budget.
        let freeze_est = freeze_estimate(est_input_tokens, merged.markup_chain.product);
        if check_daily_quota(consumed, freeze_est, daily_quota).is_err() {
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
                    code: Cow::Borrowed(codes::GW_QUOTA_BALANCE_ZERO),
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
    // 9. Hard caps (single-request size + cost).
    // ---------------------------------------------------------------------
    use gw_auth::hard_cap::{check_cost, check_input_tokens};
    if let Err(err) = check_input_tokens(est_input_tokens, hard_cap_cfg) {
        bail!(session, ctx, &err, &request_id)
    }
    // freeze-estimate uses a flat 1 micro per estimated token times the
    // markup product. The freeze is released in `logging`; Go computes the
    // real cost from the UsageRecord.
    let freeze_estimate_micro = freeze_estimate(est_input_tokens, merged.markup_chain.product);
    if let Err(err) = check_cost(freeze_estimate_micro, hard_cap_cfg) {
        bail!(session, ctx, &err, &request_id)
    }

    // ---------------------------------------------------------------------
    // 10. RequestFreeze. On drop without `release()` the destructor rolls
    //     back the frozen amount (leak-recovered metric increments).
    // ---------------------------------------------------------------------
    use gw_ledger::RequestFreeze;
    let guard = RequestFreeze::new(merged.effective_user_id.clone(), freeze_estimate_micro);
    ctx.freeze_guard = Some(guard);

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

/// Estimate a conservative freeze (in micro-units) before pricing is loaded.
/// We use a flat 1 micro per estimated token times the markup product.
fn freeze_estimate(est_input_tokens: u32, markup: f32) -> u64 {
    let base = est_input_tokens as u64;
    let with_markup = (base as f32 * markup.max(1.0)) as u64;
    with_markup.max(1)
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
