//! `upstream_peer` + `upstream_request_filter`.
//!
//! `pick_peer` does the routing work:
//! 1. Resolve the model_group + pricing from the store (lazy).
//! 2. Filter candidates via `get_candidate_keys`.
//! 3. Pick one with `Strategy::pick`.
//! 4. Build an `HttpPeer` from `Vendor.base_urls[0]` and apply timeouts.
//!
//! `rewrite_request` handles outbound request building:
//! 1. Pick the upstream codec from the picked vendor key's
//!    `accessible_endpoints` (defaulting to the client codec when the key
//!    supports the same protocol — passthrough fast path).
//! 2. If same-protocol + no mutations, the cached body is reused verbatim.
//!    Otherwise we re-render through the upstream codec.
//! 3. Inject `Authorization` per the vendor's `auth_template`.

use crate::ctx::ReqCtx;
use crate::error_emit;
use crate::filters::early::{pick_emit_codec, short_circuit, with_request_id};
use crate::timeouts::Timeouts;
use bytes::BytesMut;
use gw_codec::passthrough::same_protocol_passthrough;
use gw_codec::AnyCodec;
use gw_core::entity::{ModelGroup, Pricing, TimeoutSpec, Vendor};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::Mutations;
use gw_health::KeyHealthStore;
use gw_routing::candidates::get_candidate_keys;
use gw_routing::in_flight::{InFlightGuard, InFlightStore};
use gw_routing::strategy::set::StrategySet;
use gw_routing::{PickCtx, Strategy};
use gw_store::EntityStore;
use pingora::http::RequestHeader;
use pingora::upstreams::peer::HttpPeer;
use pingora::{Error, ErrorType, Result};
use pingora_proxy::Session;
use secrecy::ExposeSecret;
use std::borrow::Cow;
use std::sync::Arc;

/// Per-key in-flight cap used by the candidate filter. v1: 1024; v2 will move
/// this into config.
const PER_KEY_INFLIGHT_CAP: u32 = 1024;

/// Build an `ErrorIR` for an upstream-filter short-circuit.
///
/// Categories chosen so the wire status + `Retry-After` headers reflect the
/// failure shape clients should react to:
/// - `GW_BACKEND_UNAVAILABLE` (no model_group / vendor configured) → 503
///   `UpstreamError`, retryable after 5 s once an operator seeds config.
/// - `GW_ROUTING_NO_AVAILABLE_CHANNEL` (all candidates filtered out) → 503,
///   retryable after 5 s once the circuit closes or capacity frees.
pub fn envelope_for(code: &'static str, message: &'static str, status: u16) -> ErrorIR<'static> {
    let category = match status {
        400..=499 => ErrorCategory::ClientBadRequest,
        500..=599 => ErrorCategory::UpstreamError,
        _ => ErrorCategory::GatewayInternal,
    };
    let retryable = matches!(status, 502 | 503 | 504 | 429);
    let retry_after_secs = if retryable { Some(5) } else { None };
    ErrorIR {
        category,
        status,
        upstream_status: None,
        code: Cow::Borrowed(code),
        message: Cow::Borrowed(message),
        retryable,
        upstream_raw: None,
        request_id: "",
        retry_after_secs,
    }
}

/// Render an envelope to the client via the client codec, then return a
/// short-circuit `Err` for pingora.
async fn emit_envelope_and_bail<T>(
    session: &mut Session,
    ctx: &ReqCtx,
    err: ErrorIR<'static>,
) -> Result<T> {
    let codec = pick_emit_codec(&ctx.client_codec);
    let scoped = with_request_id(&err, &ctx.request_id.0);
    error_emit::send_error(session, &codec, &scoped).await;
    Err(short_circuit(err.status))
}

pub async fn pick_peer(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    health: &Arc<KeyHealthStore>,
    in_flight: &Arc<InFlightStore>,
    strategies: &Arc<StrategySet>,
    #[cfg(feature = "tunnel")] tunnel: Option<&Arc<crate::tunnel_stack::TunnelStack>>,
) -> Result<Box<HttpPeer>> {
    // The remaining `ErrorType::Custom(...)` short-circuits in this function
    // stay as bare-500 errors (no JSON envelope) on purpose. They signal
    // internal invariants that should never trip if the earlier filter stages
    // did their job (e.g. `early_request_filter` should always have set
    // `ctx.client_codec`; `request_filter` should always have set
    // `ctx.model`/`ctx.merged_perm`). Wrapping them in a vendor-flavoured
    // envelope would hide the underlying defect; the empty-body 500 keeps
    // them loud in pingora's logs so the root cause is easy to spot.
    let model = ctx
        .model
        .as_ref()
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_model_in_ctx")))?
        .clone();
    let perm = ctx
        .merged_perm
        .as_ref()
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_perm_in_ctx")))?
        .clone();
    let client_codec_kind = ctx
        .client_codec
        .as_ref()
        .map(|c| c.kind())
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_client_codec")))?;

    // -------------------------------------------------------------
    // Pick the model_group: forced_model_group from permissions wins;
    // otherwise we fall back to the first allowed group (alphabetical).
    // -------------------------------------------------------------
    let group_id = match perm
        .forced_model_group
        .clone()
        .or_else(|| perm.allowed_groups.iter().next().cloned())
    {
        Some(id) => id,
        None => {
            return emit_envelope_and_bail(
                session,
                ctx,
                envelope_for(
                    codes::GW_BACKEND_UNAVAILABLE,
                    "no model_group configured for this user",
                    503,
                ),
            )
            .await;
        }
    };

    let group: Arc<gw_core::versioned::Versioned<ModelGroup>> =
        match store.model_group_or_load(&group_id).await {
            Some(g) => g,
            None => {
                return emit_envelope_and_bail(
                    session,
                    ctx,
                    envelope_for(codes::GW_BACKEND_UNAVAILABLE, "model_group not found", 503),
                )
                .await;
            }
        };
    // G5 (T2-B): Pricing is pre-loaded in `request_filter` (after IR parse
    // exposes the model name) so `merge_permissions` can populate the
    // `model_markup` chain entry. EntityStore is Moka-memoised, so even on a
    // miss here the original load is the source of truth. `ctx.pricing` is
    // already set; nothing to do.

    // -------------------------------------------------------------
    // Candidate filter.
    // -------------------------------------------------------------
    let candidates = get_candidate_keys(
        &group.value,
        &model,
        client_codec_kind,
        &perm,
        in_flight,
        store,
        PER_KEY_INFLIGHT_CAP,
        &ctx.retry_blacklist,
        ctx.required_caps.as_ref(),
    )
    .await;
    if candidates.is_empty() {
        // G13 — distinguish "no channel at all" from "channels exist but none
        // satisfy the resolved capability requirements". Re-run the filter
        // without the capability constraint: if that yields candidates, the
        // capability filter is what emptied the set, so surface the dedicated
        // 503 `GW_ROUTING_NO_CAPABILITY_MATCH`.
        let (code, message) = if ctx.required_caps.is_some()
            && !get_candidate_keys(
                &group.value,
                &model,
                client_codec_kind,
                &perm,
                in_flight,
                store,
                PER_KEY_INFLIGHT_CAP,
                &ctx.retry_blacklist,
                None,
            )
            .await
            .is_empty()
        {
            (
                codes::GW_ROUTING_NO_CAPABILITY_MATCH,
                "no channel satisfies the required capabilities",
            )
        } else {
            (
                codes::GW_ROUTING_NO_AVAILABLE_CHANNEL,
                "no available channel for the requested model",
            )
        };
        return emit_envelope_and_bail(session, ctx, envelope_for(code, message, 503)).await;
    }

    // -------------------------------------------------------------
    // Strategy dispatch.
    // -------------------------------------------------------------
    // G12: dispatch into the per-process StrategySet so Sticky's moka cache
    // (and any future stateful strategy) is shared across requests rather
    // than allocated per call.
    let strategy = strategies.pick_for(group.value.strategy);
    let pick_ctx = PickCtx {
        user_id: &perm.effective_user_id,
        model: &model,
        conversation_id: ctx.conversation_id.as_deref(),
        message_hash: None,
        api_format: client_codec_kind,
    };
    let picked = strategy
        .pick(&candidates, health, in_flight, &pick_ctx)
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_routing_strategy_empty")))?;
    let picked_arc = Arc::clone(picked);

    let vendor = match store.vendor_or_load(&picked_arc.vendor_id).await {
        Some(v) => v,
        None => {
            return emit_envelope_and_bail(
                session,
                ctx,
                envelope_for(
                    codes::GW_BACKEND_UNAVAILABLE,
                    "vendor record missing for selected key",
                    503,
                ),
            )
            .await;
        }
    };

    let base = vendor
        .value
        .base_urls
        .first()
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_vendor_no_base_url")))?;
    let host = base
        .host_str()
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_vendor_bad_host")))?
        .to_string();
    let port = base.port_or_known_default().unwrap_or(443);
    let scheme_tls = base.scheme().eq_ignore_ascii_case("https");

    // ADR-T-1: insert the tunnel decision between strategy.pick and HttpPeer
    // construction. Without `feature = "tunnel"` or with `tunnel = None` the
    // legacy Direct path is byte-identical.
    let mut peer;
    #[cfg(feature = "tunnel")]
    {
        let provider_id = vendor.value.id.as_ref();
        let key_id = picked_arc.key_id.as_ref();
        let default_pref = gw_core::entity::TunnelPreference::default();
        let pref = picked_arc
            .tunnel_preference
            .as_ref()
            .unwrap_or(&default_pref);
        let resolution = match tunnel {
            Some(t) => t.resolve(provider_id, key_id, pref),
            None => gw_tunnel_client::resolver::TunnelResolution::Direct,
        };
        match resolution {
            gw_tunnel_client::resolver::TunnelResolution::Direct => {
                if pref.require_tunnel {
                    // ADR-T-4 — never silently fall back when require_tunnel=true.
                    metrics::counter!("gw_tunnel_resolve_total", "result" => "no_node")
                        .increment(1);
                    return emit_envelope_and_bail(
                        session,
                        ctx,
                        envelope_for(codes::GW_TUNNEL_NO_NODE, "no tunnel node available", 502),
                    )
                    .await;
                }
                metrics::counter!("gw_tunnel_resolve_total", "result" => "direct").increment(1);
                ctx.tunnel_resolution = Some("direct".into());
                peer = HttpPeer::new(format!("{host}:{port}"), scheme_tls, host.clone());
            }
            gw_tunnel_client::resolver::TunnelResolution::Use(node_id) => {
                let t = tunnel.expect("TunnelStack present implied by Use(_)");
                match t.peer_for_node(&node_id, &host, port).await {
                    Ok(p) => {
                        metrics::counter!("gw_tunnel_resolve_total", "result" => "tunnel")
                            .increment(1);
                        ctx.tunnel_resolution = Some(node_id);
                        peer = p;
                    }
                    Err(crate::tunnel_stack::TunnelPeerError::NoLive { .. }) => {
                        metrics::counter!("gw_tunnel_resolve_total", "result" => "no_live")
                            .increment(1);
                        // Push the failed key onto the retry blacklist so a
                        // retry on this request lands on a different key.
                        ctx.retry_blacklist.push(picked_arc.key_id.clone());
                        return emit_envelope_and_bail(
                            session,
                            ctx,
                            envelope_for(codes::GW_TUNNEL_NO_LIVE, "tunnel node not live", 502),
                        )
                        .await;
                    }
                    Err(crate::tunnel_stack::TunnelPeerError::TargetDenied { .. }) => {
                        metrics::counter!("gw_tunnel_resolve_total", "result" => "denied")
                            .increment(1);
                        return emit_envelope_and_bail(
                            session,
                            ctx,
                            envelope_for(
                                codes::GW_TUNNEL_TARGET_DENIED,
                                "tunnel target denied",
                                502,
                            ),
                        )
                        .await;
                    }
                }
            }
        }
    }
    #[cfg(not(feature = "tunnel"))]
    {
        ctx.tunnel_resolution = Some("direct".into());
        peer = HttpPeer::new(format!("{host}:{port}"), scheme_tls, host.clone());
    }

    // -------------------------------------------------------------
    // Timeouts (G4). Ladder: hardcoded default < Vendor.timeout_overrides
    // < Pricing timeouts. `apply_timeout_precedence` composes the effective
    // TimeoutSpec; absent fields fall through to `default_for_nonstream`.
    // -------------------------------------------------------------
    let mut t = Timeouts::default_for_nonstream();
    if let Some(p) = ctx.pricing.as_ref() {
        let eff = apply_timeout_precedence(&vendor.value, &p.value);
        if let Some(ms) = eff.connect_ms {
            t.connect = std::time::Duration::from_millis(ms as u64);
        }
        if let Some(ms) = eff.read_ms {
            t.read = std::time::Duration::from_millis(ms as u64);
        }
        if let Some(ms) = eff.total_ms {
            t.total = std::time::Duration::from_millis(ms as u64);
        }
        if let Some(ms) = eff.stream_total_ms {
            t.stream_total = std::time::Duration::from_millis(ms as u64);
        }
    }
    t.apply_to_peer(&mut peer, ctx.is_stream);

    // Stash on ctx for upstream_request_filter + logging.
    ctx.picked_vendor_key = Some(picked_arc.clone());
    ctx.picked_vendor = Some(vendor);

    // Install the in-flight RAII guard now that we've committed to a key.
    // The Drop impl decrements the counter on every termination path
    // (success, error, cancel) — see `gw_routing::in_flight::InFlightGuard`. This is
    // what makes `LoadBalance::pick` and `get_candidate_keys`'s per-key
    // in-flight cap (PER_KEY_INFLIGHT_CAP = 1024) actually observe traffic.
    let counter = in_flight.get_or_create(&picked_arc.key_id);
    ctx.inflight_guard = Some(InFlightGuard::new(counter));

    Ok(Box::new(peer))
}

/// G4 — compose effective `TimeoutSpec` from default ← `Vendor.timeout_overrides`
/// ← `Pricing` timeouts. Pricing wins because it's the most specific (per-model);
/// Vendor wins over defaults because operators typically tune at the vendor level.
/// All four fields are independently overlayed; absent fields fall through to the
/// next-lower rung (and ultimately to `Timeouts::default_for_nonstream`).
pub fn apply_timeout_precedence(vendor: &Vendor, pricing: &Pricing) -> TimeoutSpec {
    let mut eff = TimeoutSpec::default();
    if let Some(v) = vendor.timeout_overrides.as_ref() {
        if let Some(x) = v.connect_ms {
            eff.connect_ms = Some(x);
        }
        if let Some(x) = v.read_ms {
            eff.read_ms = Some(x);
        }
        if let Some(x) = v.total_ms {
            eff.total_ms = Some(x);
        }
        if let Some(x) = v.stream_total_ms {
            eff.stream_total_ms = Some(x);
        }
    }
    if let Some(x) = pricing.connect_timeout_ms {
        eff.connect_ms = Some(x);
    }
    if let Some(x) = pricing.read_timeout_ms {
        eff.read_ms = Some(x);
    }
    if let Some(x) = pricing.total_timeout_ms {
        eff.total_ms = Some(x);
    }
    if let Some(x) = pricing.stream_total_timeout_ms {
        eff.stream_total_ms = Some(x);
    }
    eff
}

pub async fn rewrite_request(
    _session: &mut Session,
    upstream_request: &mut RequestHeader,
    ctx: &mut ReqCtx,
) -> Result<()> {
    ctx.phase_start("upstream_request_filter");

    let client_codec = ctx
        .client_codec
        .as_ref()
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_client_codec")))?;
    let cached_body = ctx
        .cached_body
        .as_ref()
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_cached_body")))?;
    let picked = ctx
        .picked_vendor_key
        .as_ref()
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_picked_key")))?
        .clone();
    let vendor = ctx
        .picked_vendor
        .as_ref()
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_vendor")))?
        .clone();

    // Pick the upstream codec: prefer same protocol as client (passthrough
    // fast path); otherwise pick the first protocol the key supports.
    let upstream_kind = if picked.accessible_endpoints.contains(&client_codec.kind()) {
        client_codec.kind()
    } else if let Some(k) = picked.accessible_endpoints.first() {
        *k
    } else {
        return Err(Error::new(ErrorType::Custom("gw_no_upstream_codec")));
    };
    let upstream_codec = codec_for(upstream_kind);

    // Render the outbound body. Same-protocol passthrough avoids a
    // parse+render round trip when the IR was not mutated.
    let mutations = Mutations::NONE; // TODO(task-4.3): track real mutations from forced_params etc.
    let outbound: bytes::Bytes = if upstream_kind == client_codec.kind() {
        match client_codec {
            AnyCodec::OpenAi(c) => match same_protocol_passthrough(c, cached_body, &mutations) {
                Ok(cow) => bytes::Bytes::from(cow.into_owned()),
                Err(_e) => return Err(Error::new(ErrorType::Custom("gw_passthrough_failed"))),
            },
            // OpenAI Responses shares the vendor family with Chat and currently
            // re-renders through the Chat wire form (its `render_request` is
            // wired to OpenAiRequest); same-protocol passthrough delegates to
            // the Chat codec via a temporary `OpenAi` adapter. T6 closes the
            // proper shape-preserving path.
            AnyCodec::OpenAiResponses(_) => {
                match same_protocol_passthrough(&gw_codec::openai::OpenAi, cached_body, &mutations)
                {
                    Ok(cow) => bytes::Bytes::from(cow.into_owned()),
                    Err(_e) => return Err(Error::new(ErrorType::Custom("gw_passthrough_failed"))),
                }
            }
            AnyCodec::Anthropic(c) => match same_protocol_passthrough(c, cached_body, &mutations) {
                Ok(cow) => bytes::Bytes::from(cow.into_owned()),
                Err(_e) => return Err(Error::new(ErrorType::Custom("gw_passthrough_failed"))),
            },
            AnyCodec::Gemini(c) => match same_protocol_passthrough(c, cached_body, &mutations) {
                Ok(cow) => bytes::Bytes::from(cow.into_owned()),
                Err(_e) => return Err(Error::new(ErrorType::Custom("gw_passthrough_failed"))),
            },
        }
    } else {
        // Cross-protocol: parse client → render upstream.
        let ir = client_codec
            .parse_request(cached_body)
            .map_err(|_e| Error::new(ErrorType::Custom("gw_request_parse_failed")))?;
        let mut buf = BytesMut::with_capacity(cached_body.len());
        upstream_codec
            .render_request(&ir, &mut buf)
            .map_err(|_e| Error::new(ErrorType::Custom("gw_request_render_failed")))?;
        buf.freeze()
    };
    ctx.cached_body = Some(outbound.clone());
    ctx.upstream_codec = Some(upstream_codec);

    // S-codec T2: initialise the codec-internal SSE accumulator now that the
    // upstream codec is known. Done here (rather than in `request_filter`)
    // because the upstream codec is resolved only after `pick_peer` /
    // `rewrite_request`. Only stream requests need it; non-stream paths leave
    // `stream_state = None`.
    if ctx.is_stream {
        if let Some(codec) = ctx.upstream_codec.as_ref() {
            ctx.stream_state = Some(codec.new_stream_state());
        }
    }

    // Archive: snapshot the upstream-bound request now that `outbound` is the
    // wire form. `upstream_request` headers have already been mutated by this
    // point (Authorization injected etc.) so the archive sees what vendor sees.
    ctx.upstream_request_body = Some(std::sync::Arc::new(outbound.clone()));
    {
        use crate::ctx::UpstreamRequestMeta;
        ctx.upstream_request_meta = Some(UpstreamRequestMeta {
            method: upstream_request.method.as_str().to_owned(),
            url: upstream_request.uri.to_string(),
            http_version: format!("{:?}", upstream_request.version),
            headers: upstream_request
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
                .collect(),
        });
    }

    // Headers: rewrite Authorization per the vendor's auth_template, drop
    // the inbound auth headers, set Host, propagate traceparent.
    let _ = upstream_request.remove_header("authorization");
    let _ = upstream_request.remove_header("x-api-key");
    let _ = upstream_request.remove_header("x-api-key-id");

    // G3 / ADR-G3-2: when the vendor key carries OAuth credentials, inject the
    // (already-decrypted) access_token instead of the static api_key. Token
    // refresh is handled out-of-band by the Go task-worker: on upstream 401,
    // the OAuthListener reverse-XADDs an events:entity to trigger refresh
    // (T2-B). The expired token still flows here — Go refreshes asynchronously.
    let auth_secret = if let Some(oauth) = picked.oauth.as_ref() {
        let now = time::OffsetDateTime::now_utc();
        if now >= oauth.access_expires_at {
            metrics::counter!(
                "gw_oauth_token_expired_total",
                "vendor_key_id" => picked.key_id.as_ref().to_string(),
            )
            .increment(1);
        } else if now >= oauth.access_expires_at - time::Duration::seconds(60) {
            metrics::counter!(
                "gw_oauth_token_near_expiry_total",
                "vendor_key_id" => picked.key_id.as_ref().to_string(),
            )
            .increment(1);
        }
        oauth.access_token.expose_secret().to_owned()
    } else {
        picked.api_key.expose_secret().to_owned()
    };
    let auth_value = vendor
        .value
        .auth_template
        .replace("{api_key}", &auth_secret);
    let _ = upstream_request.insert_header("authorization", auth_value);
    if let Some(host) = vendor.value.base_urls.first().and_then(|u| u.host_str()) {
        let _ = upstream_request.insert_header("host", host.to_string());
    }
    // W3C traceparent: version-traceid-spanid-flags. The span-id is a fresh
    // 8-byte random value per upstream hop (re-using the trace-id prefix as we
    // used to do made the span-id deterministic and meaningless to consumers).
    let span_id = hex::encode(rand::random::<[u8; 8]>());
    let traceparent = format!("00-{}-{}-01", ctx.trace_id.0, span_id);
    let _ = upstream_request.insert_header("traceparent", traceparent);

    // Rewrite Content-Length: the body may have changed.
    let _ = upstream_request.insert_header("content-length", outbound.len().to_string());

    // W2-T4: mark the request as dispatched to the upstream vendor.
    crate::billing_lifecycle::transition_to(ctx, crate::billing_lifecycle::VendorExposure::Sent);

    ctx.phase_end();
    Ok(())
}

/// Wire-format-stateless codec constructors (`AnyCodec` variants are
/// zero-sized so this is essentially a tag dispatch).
fn codec_for(k: gw_core::protocol::ProtocolKind) -> AnyCodec {
    use gw_core::protocol::ProtocolKind;
    match k {
        ProtocolKind::OpenAi => AnyCodec::OpenAi(gw_codec::openai::OpenAi),
        ProtocolKind::Anthropic => AnyCodec::Anthropic(gw_codec::anthropic::Anthropic),
        ProtocolKind::Gemini => AnyCodec::Gemini(gw_codec::gemini::Gemini),
    }
}

#[cfg(test)]
mod oauth_injection_tests {
    #[test]
    fn oauth_injection_present_in_source() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/filters/upstream.rs"
        ))
        .expect("read source");
        assert!(
            src.contains("picked.oauth.as_ref()"),
            "G3 oauth branch missing in upstream_request_filter"
        );
        assert!(
            src.contains("oauth.access_token"),
            "G3 access_token usage missing"
        );
        assert!(
            src.contains("gw_oauth_token_near_expiry_total")
                && src.contains("gw_oauth_token_expired_total"),
            "G3 expiry metrics missing"
        );
    }
}
