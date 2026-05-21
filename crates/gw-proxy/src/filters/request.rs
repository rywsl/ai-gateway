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
use crate::filters::early::{client_ip_from_session, pick_emit_codec, short_circuit, with_request_id};
use gw_auth::apikey::verify_apikey;
use gw_auth::ip_filter::check_blocklist;
use gw_auth::jwt::verify_jwt;
use gw_auth::merge_permissions;
use gw_auth::ratelimit::RateLimitGuard;
use gw_auth::AuthCtx;
use gw_core::entity::ApiKeyEntry;
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{ApiKeyId, ModelName, UserId};
use time::OffsetDateTime;
use gw_store::deduction::{UserCircuit, USER_CIRCUIT};
use gw_store::{CounterStore, EntityStore};
use pingora::Result;
use pingora_proxy::Session;
use std::borrow::Cow;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

pub async fn run(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    ratelimit: &Arc<RateLimitGuard>,
    counters: &Arc<CounterStore>,
    global_rpm: u32,
    trust_xff: bool,
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
        if gw_auth::mgmt::parse_mgmt_authorization(mgmt_auth_hdr).is_none() {
            let err = ErrorIR {
                category: ErrorCategory::ClientAuth,
                status: 401,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_AUTH_INVALID_JWT),
                message: Cow::Borrowed("mgmt token required"),
                retryable: false,
                upstream_raw: None,
                request_id: "",
                retry_after_secs: None,
            };
            emit_and_bail(session, ctx, &err, &req_id_for_mgmt).await?;
            unreachable!()
        }
        // Mgmt header present and well-formed. The actual mgmt routing /
        // token validation is a follow-up task; for v1 we short-circuit
        // here so the request does not flow into business-path auth.
        // TODO(G-α.9 follow-up): dispatch into mgmt handler instead of 501.
        let err = ErrorIR {
            category: ErrorCategory::GatewayInternal,
            status: 501,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
            message: Cow::Borrowed("mgmt path not yet implemented"),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        };
        emit_and_bail(session, ctx, &err, &req_id_for_mgmt).await?;
        unreachable!()
    }

    // TODO(T6-A wave-7 wiring): once LlmGateway holds Arc<CapabilityResolver>,
    // resolve capabilities from headers + model alias + retry-upgrade hint:
    //   let header_require: Vec<&str> = session.req_header().headers
    //       .get_all("x-require-capability").iter()
    //       .filter_map(|v| v.to_str().ok())
    //       .flat_map(|s| s.split(',').map(str::trim))
    //       .collect();
    //   let req = gw_capability::resolver::ResolveRequest { ... };
    //   ctx.required_caps = Some(resolver.resolve(&req).await?);

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
    let user_id_res: Result<(UserId, Option<ApiKeyId>, Option<Vec<String>>)> = if let Some(bearer) =
        auth_header
            .as_deref()
            .and_then(|h| h.strip_prefix("Bearer "))
    {
        match verify_jwt(bearer, store).await {
            Ok(uid) => Ok((uid, None, None)),
            Err(err) => {
                emit_and_bail(session, ctx, &err, &request_id).await?;
                unreachable!()
            }
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
                            emit_and_bail(session, ctx, &err, &request_id).await?;
                            unreachable!()
                        }
                        // T6-C: stash signature requirement state so the
                        // request_body_filter (or `body_verify::verify_body`)
                        // can run body-after HMAC verification.
                        if entry.value.signature_required {
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
                            // G7 v1.5 — reuse the api-key hash as the HMAC
                            // secret. A dedicated `signature_secret` field
                            // on `ApiKeyEntry` is a future follow-up.
                            ctx.signature_secret = entry.value.hash.clone();
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
                        emit_and_bail(session, ctx, &err, &request_id).await?;
                        unreachable!()
                    }
                }
            }
            Err(err) => {
                emit_and_bail(session, ctx, &err, &request_id).await?;
                unreachable!()
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
        emit_and_bail(session, ctx, &err, &request_id).await?;
        unreachable!()
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
            emit_and_bail(session, ctx, &err, &request_id).await?;
            unreachable!()
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
            emit_and_bail(session, ctx, &err, &request_id).await?;
            unreachable!()
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
            emit_and_bail(session, ctx, &err, &request_id).await?;
            unreachable!()
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
            emit_and_bail(session, ctx, &err, &request_id).await?;
            unreachable!()
        }
        body_buf.extend_from_slice(&chunk);
    }
    let body_bytes = bytes::Bytes::from(body_buf);
    ctx.cached_body = Some(body_bytes.clone());

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
            secret: ctx.signature_secret.clone(),
            signature: ctx.signature_received.clone(),
            timestamp: ctx.signature_timestamp.clone(),
        };
        match crate::body_verify::verify_body(input) {
            Ok((SignatureCheckResult::Valid, _))
            | Ok((SignatureCheckResult::NotRequired, _)) => {
                // Pass through. `cached_body` already holds the on-wire form
                // the codec parse step expects; decompression for verification
                // is internal to `verify_body` and intentionally not folded
                // back into `cached_body` (the codec handles Content-Encoding
                // on its own path).
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
                emit_and_bail(session, ctx, &err, &request_id).await?;
                unreachable!()
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
                emit_and_bail(session, ctx, &err, &request_id).await?;
                unreachable!()
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
                emit_and_bail(session, ctx, &err, &request_id).await?;
                unreachable!()
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
                emit_and_bail(session, ctx, &err, &request_id).await?;
                unreachable!()
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
            emit_and_bail(session, ctx, &err, &request_id).await?;
            unreachable!()
        }
    };
    let (model_name, is_stream, est_input_tokens) = match codec.parse_request(&body_bytes) {
        Ok(ir) => (
            ModelName::from(ir.model.as_ref()),
            ir.stream,
            codec.estimate_input_tokens(&ir),
        ),
        Err(err) => {
            emit_and_bail(session, ctx, &err, &request_id).await?;
            unreachable!()
        }
    };
    ctx.model = Some(model_name.clone());
    ctx.is_stream = is_stream;
    // POSTs that mutate vendor state are *not* idempotent. We can refine
    // this once the codec exposes "method classification"; for v1 mark
    // non-stream completions as idempotent and stream/tool requests as not.
    ctx.idempotent = !is_stream;

    // ---------------------------------------------------------------------
    // 4b. v1 limitation: reject non-stream completions outright. The codec
    //     `parse_response` method is `unimplemented!()` for every protocol
    //     in v1 scope, which means a non-stream path through `logging`
    //     would release the freeze (Rust never computes cost; cost lives in Go)
    //     and the user would be billed nothing. Surfacing a clear 400 here
    //     fails closed and avoids silent free traffic; v1.1 lifts this once
    //     `parse_response` lands per the codec contract.
    // ---------------------------------------------------------------------
    if !is_stream {
        let err = ErrorIR {
            category: ErrorCategory::ClientBadRequest,
            status: 400,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_REQUEST_NONSTREAM_NOT_SUPPORTED),
            message: Cow::Borrowed(
                "v1 gateway requires stream=true on every request; \
                 non-stream completions will land in v1.1",
            ),
            retryable: false,
            upstream_raw: None,
            request_id: "",
            retry_after_secs: None,
        };
        emit_and_bail(session, ctx, &err, &request_id).await?;
        unreachable!()
    }

    // ---------------------------------------------------------------------
    // 5. Merge permissions across the five-tier hierarchy.
    // ---------------------------------------------------------------------
    let auth_ctx = AuthCtx {
        user_id: user_id.clone(),
        api_key_id: api_key_id.clone(),
        client_ip: client_ip_v.unwrap_or(IpAddr::from([0, 0, 0, 0])),
    };
    let merged = match merge_permissions(&auth_ctx, store, global_rpm).await {
        Ok(p) => p,
        Err(err) => {
            emit_and_bail(session, ctx, &err, &request_id).await?;
            unreachable!()
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
        emit_and_bail(session, ctx, &err, &request_id).await?;
        unreachable!()
    }

    // ---------------------------------------------------------------------
    // 7. Balance enforcement. Read the payer's `balance_micro` counter from
    //     Redis and reject when it has fallen to zero or below. This is the
    //     primary zero-balance gate; the USER_CIRCUIT check below is a
    //     fast-path follow-up that catches users whose circuit was tripped
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
        emit_and_bail(session, ctx, &err, &request_id).await?;
        unreachable!()
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
                emit_and_bail(session, ctx, &err, &request_id).await?;
                unreachable!()
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
            emit_and_bail(session, ctx, &err, &request_id).await?;
            unreachable!()
        }
    }
    if is_stream {
        match ratelimit.check_stream_slot(&merged.effective_user_id, merged.max_concurrent_streams)
        {
            Ok(slot) => ctx.stream_slot = Some(slot),
            Err(err) => {
                emit_and_bail(session, ctx, &err, &request_id).await?;
                unreachable!()
            }
        }
    }

    // ---------------------------------------------------------------------
    // 9. Hard caps (single-request size + cost).
    // ---------------------------------------------------------------------
    use gw_auth::hard_cap::{check_cost, check_input_tokens, HardCapCfg};
    let hc_cfg = HardCapCfg {
        max_input_tokens: 1_000_000,
        max_cost_micro: 5_000_000_000,
    };
    if let Err(err) = check_input_tokens(est_input_tokens, &hc_cfg) {
        emit_and_bail(session, ctx, &err, &request_id).await?;
        unreachable!()
    }
    // freeze-estimate uses a flat 1 micro per estimated token times the
    // markup product. The freeze is released in `logging`; Go computes the
    // real cost from the UsageRecord.
    let freeze_estimate_micro = freeze_estimate(est_input_tokens, merged.markup_chain.product);
    if let Err(err) = check_cost(freeze_estimate_micro, &hc_cfg) {
        emit_and_bail(session, ctx, &err, &request_id).await?;
        unreachable!()
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

/// Estimate a conservative freeze (in micro-units) before pricing is loaded.
/// We use a flat 1 micro per estimated token times the markup product.
fn freeze_estimate(est_input_tokens: u32, markup: f32) -> u64 {
    let base = est_input_tokens as u64;
    let with_markup = (base as f32 * markup.max(1.0)) as u64;
    with_markup.max(1)
}

/// Emit an error via the client codec and return a short-circuit Err so the
/// `?` operator unwinds. The caller writes `unreachable!()` after the await
/// because `Err` already terminates the function on the call site.
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
