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
use crate::filters::early::{pick_emit_codec, short_circuit, with_request_id};
use gw_auth::apikey::verify_apikey;
use gw_auth::ip_filter::check_blocklist;
use gw_auth::jwt::verify_jwt;
use gw_auth::merge_permissions;
use gw_auth::ratelimit::RateLimitGuard;
use gw_auth::AuthCtx;
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{ApiKeyId, ModelName, UserId};
use gw_store::deduction::{UserCircuit, USER_CIRCUIT};
use gw_store::EntityStore;
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
    global_rpm: u32,
) -> Result<bool> {
    ctx.phase_start("request_filter");
    ctx.timing.start = Some(Instant::now());

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
    let user_id_res: Result<(UserId, Option<ApiKeyId>)> = if let Some(bearer) = auth_header
        .as_deref()
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        match verify_jwt(bearer, store).await {
            Ok(uid) => Ok((uid, None)),
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
                match store.apikey.get(&akid).await {
                    Some(entry) => Ok((entry.value.user_id.clone(), Some(akid))),
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
    let (user_id, api_key_id) = user_id_res?;

    // Load the user entry (used by ReqCtx for downstream consumers).
    let user_entry = match store.user.get(&user_id).await {
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

    // ---------------------------------------------------------------------
    // 2. Re-run IP blocklist with the resolved scope IDs.
    // ---------------------------------------------------------------------
    let client_ip_v = client_ip(session);
    if let Some(ip) = client_ip_v {
        if let Err(err) = check_blocklist(ip, Some(&user_id), api_key_id.as_ref(), store).await {
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
    // 7. USER_CIRCUIT breaker (zero-balance / banned-user short-circuit).
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
    // freeze-estimate is computed once we have pricing; for now we freeze a
    // conservative placeholder so the user is gated against runaway parallel
    // requests. The real per-token cost is settled in `logging`.
    let freeze_estimate_micro = freeze_estimate(est_input_tokens, &merged.markup_product);
    if let Err(err) = check_cost(freeze_estimate_micro, &hc_cfg) {
        emit_and_bail(session, ctx, &err, &request_id).await?;
        unreachable!()
    }

    // ---------------------------------------------------------------------
    // 10. FreezeGuard. On drop without `settle` the destructor rolls back
    //     the frozen amount.
    // ---------------------------------------------------------------------
    use gw_ledger::FreezeGuard;
    let guard = FreezeGuard::freeze(merged.effective_user_id.clone(), freeze_estimate_micro);
    ctx.freeze_guard = Some(guard);

    ctx.merged_perm = Some(merged);
    ctx.phase_end();
    Ok(false)
}

/// Estimate a conservative freeze (in micro-units) before pricing is loaded.
/// We use a flat 1 micro per estimated token times the markup product.
fn freeze_estimate(est_input_tokens: u32, markup: &f32) -> u64 {
    let base = est_input_tokens as u64;
    let with_markup = (base as f32 * markup.max(1.0)) as u64;
    with_markup.max(1)
}

fn client_ip(session: &Session) -> Option<IpAddr> {
    session.client_addr().and_then(|sa| match sa {
        pingora::protocols::l4::socket::SocketAddr::Inet(addr) => Some(addr.ip()),
        _ => None,
    })
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
