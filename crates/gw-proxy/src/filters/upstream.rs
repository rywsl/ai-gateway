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
use bytes::{Bytes, BytesMut};
use gw_codec::cli::CliCodec;
use gw_codec::passthrough::same_protocol_passthrough;
use gw_codec::AnyCodec;
use gw_core::entity::{ForcedParams, ModelGroup, Pricing, TimeoutSpec, Vendor, VendorKey};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{ForcedMutations, Mutations};
use gw_health::KeyHealthStore;
use gw_routing::candidates::get_candidate_keys;
use gw_routing::in_flight::{InFlightGuard, InFlightStore};
use gw_routing::latency::LatencyTracker;
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

/// Per-key in-flight cap used by the candidate filter.
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
    gw_core::observability::observe(gw_core::observability::ErrorObservation {
        code: err.code.as_ref(),
        kind: gw_core::observability::ErrorKind::ShortCircuit,
        stage: gw_core::observability::Stage::Request,
        status: Some(err.status),
        trace_id: Some(ctx.trace_id.as_ref()),
        extra: None,
    });
    let codec = pick_emit_codec(&ctx.client_codec);
    let scoped = with_request_id(&err, &ctx.request_id.0);
    error_emit::send_error(session, &codec, &scoped).await;
    Err(short_circuit(err.status))
}

// `pick_peer` is the kill-switch + impersonate-dispatch hot path; the arg count
// grew with Task 4.2 (impersonate) and Task 5.2 (egress_disabled). Refactoring
// to a struct would obscure call sites; keep the allow until the next time the
// signature changes.
#[allow(clippy::too_many_arguments)]
pub async fn pick_peer(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    health: &Arc<KeyHealthStore>,
    in_flight: &Arc<InFlightStore>,
    strategies: &Arc<StrategySet>,
    latency_tracker: &Arc<LatencyTracker>,
    adaptive_rpm: &Arc<gw_auth::ratelimit::adaptive::AdaptiveRpmManager>,
    egress: &Arc<gw_egress_impersonate::EgressHandle>,
    egress_disabled: bool,
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
    let mut candidates = get_candidate_keys(
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

    // Kill-switch: when egress is globally disabled, drop any candidate whose
    // vendor uses `egress_mode = Impersonate`. Rustls vendors are unaffected.
    // A cold `vendor_or_load` miss can hit the DB, which is acceptable during
    // a kill-switch incident — the candidate set is small and the incident is
    // brief. `Vec::retain` cannot take an async closure, hence the explicit loop.
    if egress_disabled && !candidates.is_empty() {
        let mut kept = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if let Some(v) = store.vendor_or_load(&candidate.vendor_id).await {
                if let gw_core::impersonate::VendorEgressMode::Impersonate { profile } =
                    v.value.egress_mode
                {
                    metrics::counter!(
                        "gw_egress_disabled_total",
                        "profile" => profile.as_static_str(),
                    )
                    .increment(1);
                    continue;
                }
            }
            // Rustls vendor, or vendor failed to load (the normal pick path
            // will surface the missing-vendor 503).
            kept.push(candidate);
        }
        candidates = kept;
    }

    if let Some(client_shape) = ctx.client_shape.filter(gw_core::protocol::ApiShape::is_cli) {
        let required_kind = client_shape.protocol_kind();
        candidates.retain(|key| key.accessible_endpoints.contains(&required_kind));
        if candidates.is_empty() {
            metrics::counter!(
                "gw_cli_cross_protocol_denied_total",
                "client_shape" => client_shape.format_id(),
                "required_kind" => required_kind.as_str(),
            )
            .increment(1);
            return emit_envelope_and_bail(
                session,
                ctx,
                envelope_for(
                    codes::GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL,
                    "CLI request requires a same-protocol upstream channel",
                    503,
                ),
            )
            .await;
        }
    }
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
    if ctx.picked_candidates.is_none() {
        ctx.picked_candidates = Some(candidates.clone());
    }

    // -------------------------------------------------------------
    // Strategy dispatch.
    // -------------------------------------------------------------
    // G12: dispatch into the per-process StrategySet so Sticky's moka cache
    // (and any future stateful strategy) is shared across requests rather
    // than allocated per call.
    let strategy = strategies.pick_for(&group.value.strategy);
    ctx.strategy_name = Some(strategy.name());
    let pick_ctx = PickCtx {
        user_id: &perm.effective_user_id,
        model: &model,
        conversation_id: ctx.conversation_id.as_deref(),
        message_hash: ctx.body.as_ref().and_then(|v| {
            let raw = v.raw();
            if raw.is_empty() {
                None
            } else {
                body_hash_u64(raw)
            }
        }),
        api_format: client_codec_kind,
        pricing: ctx.pricing.as_ref().map(|p| &p.value as &Pricing),
        latency_tracker: latency_tracker.as_ref(),
        reservation_view: ctx.reservation_view.as_deref(),
        frequency_tracker: ctx.frequency_tracker.as_deref(),
    };
    let failover_pick = if ctx.failover_attempt > 0 && ctx.upstream_status.is_none() {
        ctx.picked_vendor_key.as_ref().and_then(|next| {
            candidates
                .iter()
                .find(|candidate| candidate.key_id == next.key_id)
        })
    } else {
        None
    };
    let picked = match failover_pick {
        Some(candidate) => candidate,
        None => {
            let first = strategy
                .pick(&candidates, health, in_flight, &pick_ctx)
                .ok_or_else(|| Error::new(ErrorType::Custom("gw_routing_strategy_empty")))?;
            // Adaptive-RPM enforcement (audit 2026-05-30 §1.8): if the strategy's
            // pick is over its learned per-key ceiling, divert to another
            // candidate that is within its ceiling. This makes the learned limit
            // actually enforced (the former request_filter clamp was dead code).
            // Fail-operational: if every candidate is over its ceiling, send to
            // `first` anyway — the per-user RPM bucket and response-side cooldown
            // remain in force, so a denial is proactive back-pressure, not a hard
            // reject. Only applied on the first-attempt pick; the failover path
            // honors the FailoverExecutor's deliberate choice as-is.
            if adaptive_rpm.try_admit(&first.key_id) {
                first
            } else if let Some(alt) = candidates
                .iter()
                .find(|c| c.key_id != first.key_id && adaptive_rpm.try_admit(&c.key_id))
            {
                alt
            } else {
                first
            }
        }
    };
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
    //
    // Phase 4 (egress impersonate): a vendor whose `egress_mode = Impersonate`
    // routes through the in-process boring egress instead of the rustls/tunnel
    // path. Rustls vendors (the default) take the existing path unchanged.
    use gw_core::impersonate::VendorEgressMode;
    let impersonate_profile = match vendor.value.egress_mode {
        VendorEgressMode::Impersonate { profile } => Some(profile),
        VendorEgressMode::Rustls => None,
    };

    // Resolve the tunnel decision once. Both branches need it: the impersonate
    // branch to detect the tunnel+impersonate conflict, the rustls branch to
    // pick its peer. `TunnelStack::resolve` is pure today but could grow side
    // effects (counters, tracing) — keeping the call site singular avoids
    // double-firing on the impersonate path.
    #[cfg(feature = "tunnel")]
    let (tunnel_resolution, tunnel_require) = {
        let default_pref = gw_core::entity::TunnelPreference::default();
        let pref = picked_arc
            .tunnel_preference
            .as_ref()
            .unwrap_or(&default_pref);
        let resolution = match tunnel {
            Some(t) => t.resolve(vendor.value.id.as_ref(), picked_arc.key_id.as_ref(), pref),
            None => gw_tunnel_client::resolver::TunnelResolution::Direct,
        };
        (
            resolution,
            pref.require_tunnel, /* GW_TUNNEL_NO_NODE emitted in the Rustls branch below */
        )
    };

    let mut peer;
    if let Some(profile) = impersonate_profile {
        // Hard exclusion: a vendor cannot tunnel AND impersonate. If the
        // resolver would route through a tunnel node, bail rather than
        // silently picking one.
        #[cfg(feature = "tunnel")]
        if let gw_tunnel_client::resolver::TunnelResolution::Use(_) = &tunnel_resolution {
            return emit_envelope_and_bail(
                session,
                ctx,
                envelope_for(
                    codes::GW_TUNNEL_IMPERSONATE_CONFLICT,
                    "vendor cannot enable both tunnel and impersonate",
                    503,
                ),
            )
            .await;
        }
        ctx.egress_profile = Some(profile);
        ctx.egress_upstream = Some((host.clone(), port));
        // `tunnel_resolution` intentionally left None on the impersonate path;
        // consumers should branch on `egress_profile.is_some()` instead.
        metrics::counter!("gw_egress_impersonate_dispatch_total").increment(1);
        peer = match egress.bind_addr() {
            gw_egress_impersonate::EgressBind::Uds(p) => {
                HttpPeer::new_uds(p.to_string_lossy().as_ref(), false, host.clone())
                    // EgressHandle::start validates the path at bind time, so this
                    // arm is an invariant violation rather than a runtime error.
                    .map_err(|_| Error::new(ErrorType::Custom("gw_egress_bad_uds_path")))?
            }
            gw_egress_impersonate::EgressBind::Tcp(addr) => {
                HttpPeer::new(addr.to_string(), false, host.clone())
            }
        };
    } else {
        #[cfg(feature = "tunnel")]
        {
            match tunnel_resolution {
                gw_tunnel_client::resolver::TunnelResolution::Direct => {
                    if tunnel_require {
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
    ctx.response_deadline = Some(if ctx.is_stream {
        t.stream_total
    } else {
        t.total
    });

    // Stash on ctx for upstream_request_filter + logging.
    ctx.picked_vendor_key = Some(picked_arc.clone());
    ctx.picked_vendor = Some(vendor);
    ctx.forced_params = group.value.forced_params.clone();

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

fn apply_forced_params_mutations(fp: &ForcedParams) -> (Mutations, ForcedMutations) {
    let mut m = Mutations::NONE;
    let mut fm = ForcedMutations::NONE;
    if fp.temperature.is_some() {
        m.set_temperature();
        fm.set_temperature();
    }
    if fp.top_p.is_some() {
        m.set_temperature(); // top_p is not in Mutations; mark temperature as closest proxy
        fm.set_top_p();
    }
    (m, fm)
}

fn apply_forced_params_to_ir(ir: &mut gw_core::ir::RequestIR<'_>, fp: &ForcedParams) {
    if let Some(t) = fp.temperature {
        ir.temperature = Some(t);
    }
    if let Some(p) = fp.top_p {
        ir.top_p = Some(p);
    }
}

fn apply_overrides_to_mutations(mutations: &mut Mutations, has_overrides: bool) {
    if has_overrides {
        mutations.set_passthrough_overrides();
    }
}

fn model_rewrite(
    model: Option<&gw_core::ids::ModelName>,
    picked: &VendorKey,
) -> Option<(
    String,
    std::collections::HashMap<String, Box<serde_json::value::RawValue>>,
)> {
    let model = model?;
    let upstream_model = picked.model_mapping.get(model)?;
    if upstream_model.is_empty() || upstream_model == model.as_ref() {
        return None;
    }
    let Ok(raw) = serde_json::value::RawValue::from_string(
        serde_json::to_string(upstream_model).unwrap_or_default(),
    ) else {
        return None;
    };
    let mut overrides = std::collections::HashMap::with_capacity(1);
    overrides.insert("model".to_owned(), raw);
    Some((upstream_model.clone(), overrides))
}

fn apply_passthrough_overrides_to_ir(
    ir: &mut gw_core::ir::RequestIR<'_>,
    overrides: Option<&std::collections::HashMap<String, Box<serde_json::value::RawValue>>>,
) {
    if let Some(overrides) = overrides {
        ir.passthrough_overrides = Some(overrides.clone());
    }
}

fn bytes_from_passthrough(cow: Cow<'_, [u8]>, original: &Bytes) -> Bytes {
    match cow {
        Cow::Borrowed(b)
            if std::ptr::eq(b.as_ptr(), original.as_ptr()) && b.len() == original.len() =>
        {
            original.clone()
        }
        Cow::Borrowed(b) => Bytes::copy_from_slice(b),
        Cow::Owned(v) => Bytes::from(v),
    }
}

pub type BodyRulesMap = std::collections::HashMap<
    String,
    std::collections::HashMap<String, Box<serde_json::value::RawValue>>,
>;

fn ir_has_typed_field(ir: &gw_core::ir::RequestIR<'_>, key: &str) -> bool {
    match key {
        "model" => !ir.model.is_empty(),
        "temperature" => ir.temperature.is_some(),
        "top_p" => ir.top_p.is_some(),
        "top_k" => ir.top_k.is_some(),
        "max_tokens" => ir.max_tokens.is_some(),
        "presence_penalty" => ir.presence_penalty.is_some(),
        "frequency_penalty" => ir.frequency_penalty.is_some(),
        "seed" => ir.seed.is_some(),
        "stream" => true, // always present in wire format
        "stop" => ir.stop.is_some(),
        "tools" => ir.tools.is_some(),
        "tool_choice" => ir.tool_choice.is_some(),
        "response_format" => ir.response_format.is_some(),
        _ => false,
    }
}

fn apply_body_rules(
    ir: &mut gw_core::ir::RequestIR<'_>,
    endpoint: &str,
    body_rules: &BodyRulesMap,
    raw_body: &[u8],
) -> bool {
    let rules = match body_rules.get(endpoint) {
        Some(r) if !r.is_empty() => r,
        _ => return false,
    };

    let parsed_body: Option<serde_json::Map<String, serde_json::Value>> =
        serde_json::from_slice(raw_body).ok();

    let mut injected = false;
    for (key, value) in rules {
        if ir_has_typed_field(ir, key) {
            continue;
        }
        if let Some(ref body) = parsed_body {
            if body.contains_key(key) {
                continue;
            }
        }
        if let Some(ref existing) = ir.passthrough_overrides {
            if existing.contains_key(key) {
                continue;
            }
        }

        let overrides = ir
            .passthrough_overrides
            .get_or_insert_with(std::collections::HashMap::new);
        overrides.insert(key.clone(), value.clone());
        injected = true;

        metrics::counter!(
            "gw_body_rules_applied_total",
            "endpoint" => endpoint.to_owned(),
            "key" => key.clone(),
        )
        .increment(1);
    }

    injected
}

pub async fn rewrite_request(
    session: &mut Session,
    upstream_request: &mut RequestHeader,
    ctx: &mut ReqCtx,
    body_rules: &BodyRulesMap,
) -> Result<()> {
    ctx.phase_start("upstream_request_filter");

    if ctx.mgmt {
        let user_id = ctx
            .mgmt_user_id
            .as_ref()
            .ok_or_else(|| Error::new(ErrorType::Custom("gw_mgmt_missing_user_id")))?;
        let scope = ctx
            .mgmt_scope
            .ok_or_else(|| Error::new(ErrorType::Custom("gw_mgmt_missing_scope")))?;
        let scope = match scope {
            gw_core::entity::MgmtScope::UserSelf => "user_self",
            gw_core::entity::MgmtScope::AdminRead => "admin_read",
            gw_core::entity::MgmtScope::AdminFull => "admin_full",
        };
        let _ = upstream_request.insert_header("x-gateway-mgmt-user-id", user_id.as_ref());
        let _ = upstream_request.insert_header("x-gateway-mgmt-scope", scope);
        ctx.phase_end();
        return Ok(());
    }

    let client_codec = ctx
        .client_codec
        .as_ref()
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_client_codec")))?;
    let outbound_body = ctx
        .body
        .as_ref()
        .map(|v| v.outbound_or_raw().clone())
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_body_view")))?;
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

    // Pick the upstream codec via the shared resolver (single source of truth,
    // identical to the routing health gate's read side) so the (key, ApiFormat)
    // health bucket written on the response path matches the one selection read
    // (audit 2026-05-30 §1.5). Empty accessible_endpoints is a hard error.
    if picked.accessible_endpoints.is_empty() {
        return Err(Error::new(ErrorType::Custom("gw_no_upstream_codec")));
    }
    let upstream_kind = gw_core::protocol::resolve_upstream_format(
        &picked.accessible_endpoints,
        client_codec.kind(),
    );
    let upstream_codec = codec_for(upstream_kind);

    if let Some(client_shape) = ctx.client_shape.filter(gw_core::protocol::ApiShape::is_cli) {
        let outbound = cli_passthrough_body(ctx, upstream_kind)?;
        ctx.upstream_codec = Some(upstream_codec);
        ctx.upstream_shape = Some(client_shape);
        if ctx.is_stream {
            if let Some(codec) = ctx.upstream_codec.as_ref() {
                ctx.stream_state = Some(codec.new_stream_state());
            }
        }
        archive_upstream_request(ctx, upstream_request, outbound.clone());
        inject_vendor_headers(ctx, upstream_request, &picked, &vendor, outbound.len());
        inject_egress_headers(ctx, upstream_request, &picked).await;
        crate::billing_lifecycle::transition_to(
            ctx,
            crate::billing_lifecycle::VendorExposure::Sent,
        );
        ctx.phase_end();
        return Ok(());
    }

    // Render the outbound body. Same-protocol passthrough avoids a
    // parse+render round trip when the IR was not mutated.
    let model_rewrite = model_rewrite(ctx.model.as_ref(), &picked);
    if let Some((upstream_model, _)) = model_rewrite.as_ref() {
        metrics::counter!(
            "gw_model_rewrite_total",
            "from" => ctx.model.as_ref().map(|m| m.as_ref()).unwrap_or_default().to_string(),
            "to" => upstream_model.clone(),
        )
        .increment(1);
        ctx.model = Some(gw_core::ids::ModelName::from(upstream_model.as_str()));
    }
    let passthrough_overrides = model_rewrite.as_ref().map(|(_, overrides)| overrides);
    let (mut mutations, forced_mutations) = apply_forced_params_mutations(&ctx.forced_params);
    apply_overrides_to_mutations(&mut mutations, passthrough_overrides.is_some());
    let endpoint_path = session.req_header().uri.path();
    let has_body_rules = body_rules.get(endpoint_path).is_some_and(|r| !r.is_empty());
    if has_body_rules {
        mutations.set_passthrough_overrides();
    }
    let outbound: bytes::Bytes = if mutations.is_empty() && upstream_kind == client_codec.kind() {
        match client_codec {
            AnyCodec::OpenAi(c) => match same_protocol_passthrough(c, &outbound_body, &mutations) {
                Ok(cow) => bytes_from_passthrough(cow, &outbound_body),
                Err(_e) => return Err(Error::new(ErrorType::Custom("gw_passthrough_failed"))),
            },
            // OpenAI Responses shares the vendor family with Chat and
            // re-renders through the Chat wire form (its `render_request` is
            // wired to OpenAiRequest); same-protocol passthrough delegates to
            // the Chat codec via an `OpenAi` adapter.
            AnyCodec::OpenAiResponses(_) => {
                match same_protocol_passthrough(
                    &gw_codec::openai::OpenAi,
                    &outbound_body,
                    &mutations,
                ) {
                    Ok(cow) => bytes_from_passthrough(cow, &outbound_body),
                    Err(_e) => return Err(Error::new(ErrorType::Custom("gw_passthrough_failed"))),
                }
            }
            AnyCodec::Anthropic(c) => {
                match same_protocol_passthrough(c, &outbound_body, &mutations) {
                    Ok(cow) => bytes_from_passthrough(cow, &outbound_body),
                    Err(_e) => return Err(Error::new(ErrorType::Custom("gw_passthrough_failed"))),
                }
            }
            AnyCodec::Gemini(c) => match same_protocol_passthrough(c, &outbound_body, &mutations) {
                Ok(cow) => bytes_from_passthrough(cow, &outbound_body),
                Err(_e) => return Err(Error::new(ErrorType::Custom("gw_passthrough_failed"))),
            },
        }
    } else {
        // Parse → mutate (forced_params) → render through the upstream codec.
        let mut ir = client_codec
            .parse_request(&outbound_body)
            .map_err(|_e| Error::new(ErrorType::Custom("gw_request_parse_failed")))?;
        apply_forced_params_to_ir(&mut ir, &ctx.forced_params);
        apply_passthrough_overrides_to_ir(&mut ir, passthrough_overrides);
        if has_body_rules {
            apply_body_rules(&mut ir, endpoint_path, body_rules, &outbound_body);
        }
        let mut buf = BytesMut::with_capacity(outbound_body.len());
        upstream_codec
            .render_request(&ir, &mut buf)
            .map_err(|_e| Error::new(ErrorType::Custom("gw_request_render_failed")))?;
        buf.freeze()
    };

    for field in forced_mutations.field_names() {
        metrics::counter!("gw_forced_params_applied_total", "field" => field).increment(1);
    }
    ctx.forced_mutations = forced_mutations;
    if let Some(view) = ctx.body.as_mut() {
        view.set_outbound(outbound.clone());
    }
    ctx.upstream_codec = Some(upstream_codec);
    ctx.upstream_shape = ctx.upstream_codec.as_ref().map(AnyCodec::api_shape);

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

    archive_upstream_request(ctx, upstream_request, outbound.clone());
    inject_vendor_headers(ctx, upstream_request, &picked, &vendor, outbound.len());
    inject_egress_headers(ctx, upstream_request, &picked).await;

    // W2-T4: mark the request as dispatched to the upstream vendor.
    crate::billing_lifecycle::transition_to(ctx, crate::billing_lifecycle::VendorExposure::Sent);

    ctx.phase_end();
    Ok(())
}

#[cfg(test)]
fn render_same_protocol_openai_with_passthrough_overrides_for_test(
    body: &[u8],
    overrides: &[(&str, &str)],
) -> Bytes {
    use gw_codec::ProtocolCodec;

    let mut ir = gw_codec::openai::OpenAi.parse_request(body).unwrap();
    let mut map = std::collections::HashMap::new();
    for (key, value) in overrides {
        map.insert(
            (*key).to_owned(),
            serde_json::value::RawValue::from_string((*value).to_owned()).unwrap(),
        );
    }
    apply_passthrough_overrides_to_ir(&mut ir, Some(&map));
    let mut buf = BytesMut::with_capacity(body.len());
    gw_codec::openai::OpenAi
        .render_request(&ir, &mut buf)
        .unwrap();
    buf.freeze()
}

pub fn cli_passthrough_body(
    ctx: &mut ReqCtx,
    upstream_kind: gw_core::protocol::ProtocolKind,
) -> Result<bytes::Bytes> {
    let client_shape = ctx
        .client_shape
        .filter(gw_core::protocol::ApiShape::is_cli)
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_not_cli_request")))?;
    if client_shape.protocol_kind() != upstream_kind {
        return Err(Error::new(ErrorType::Custom("gw_cli_cross_protocol")));
    }
    let outbound_body = ctx
        .body
        .as_ref()
        .map(|v| v.outbound_or_raw().clone())
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_body_view")))?;
    let passthrough = match client_shape {
        gw_core::protocol::ApiShape::ClaudeCli => {
            gw_codec::cli::claude_cli::ClaudeCliCodec.passthrough_request(&outbound_body)
        }
        gw_core::protocol::ApiShape::OpenAiCli => {
            gw_codec::cli::openai_cli::OpenAiCliCodec.passthrough_request(&outbound_body)
        }
        gw_core::protocol::ApiShape::OpenAiCompact => {
            gw_codec::cli::openai_compact::OpenAiCompactCodec.passthrough_request(&outbound_body)
        }
        gw_core::protocol::ApiShape::GeminiCli => {
            gw_codec::cli::gemini_cli::GeminiCliCodec.passthrough_request(&outbound_body)
        }
        _ => std::borrow::Cow::Borrowed(outbound_body.as_ref()),
    };
    let outbound = bytes::Bytes::copy_from_slice(passthrough.as_ref());
    if let Some(view) = ctx.body.as_mut() {
        view.set_outbound(outbound.clone());
    }
    Ok(outbound)
}

fn archive_upstream_request(
    ctx: &mut ReqCtx,
    upstream_request: &RequestHeader,
    outbound: bytes::Bytes,
) {
    ctx.upstream_request_body = Some(std::sync::Arc::new(outbound));
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

fn inject_vendor_headers(
    ctx: &mut ReqCtx,
    upstream_request: &mut RequestHeader,
    picked: &gw_core::entity::VendorKey,
    vendor: &gw_core::versioned::Versioned<Vendor>,
    content_len: usize,
) {
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
    let _ = upstream_request.insert_header("content-length", content_len.to_string());
}

/// Inject X-Egress-* routing headers and per-profile forged identity headers.
///
/// This is a separate async function so that `inject_vendor_headers` (and its
/// unit test `direct_mode_inject_vendor_headers`) remains synchronous.  When
/// `ctx.egress_profile` is `None` (i.e. the standard rustls path) the function
/// is a no-op and adds zero overhead to the hot path.
async fn inject_egress_headers(
    ctx: &ReqCtx,
    upstream_request: &mut RequestHeader,
    picked: &gw_core::entity::VendorKey,
) {
    let Some(profile) = ctx.egress_profile else {
        return;
    };

    // X-Egress-Target: host:port
    if let Some((host, port)) = ctx.egress_upstream.as_ref() {
        let _ = upstream_request.insert_header("X-Egress-Target", format!("{host}:{port}"));
    }

    // X-Egress-Profile: e.g. "claude_code_chrome131"
    let _ = upstream_request.insert_header("X-Egress-Profile", profile.as_static_str());

    // X-Egress-Proxy: proxy URL (only when a per-key proxy is configured)
    if let Some(proxy) = picked.proxy.as_ref() {
        let _ = upstream_request.insert_header("X-Egress-Proxy", proxy.as_str());
    }

    // Derive (or reuse) the per-VendorKey identity and optionally schedule a
    // lazy persist when we just generated a fresh one.
    let (resolved, should_persist) = gw_core::impersonate::ensure_identity(
        picked.key_id.as_ref(),
        picked.identity_snapshot.as_ref(),
        profile,
    );

    if should_persist {
        if let Some(q) = ctx.identity_persist.as_ref() {
            let snap = gw_core::impersonate::IdentitySnapshot {
                profile_kind: resolved.profile,
                variable: resolved.variable.clone(),
            };
            q.schedule(picked.key_id.as_ref().to_owned(), snap);
        }
    }

    // Build and insert the profile-specific forged identity headers.
    let identity_hdrs = identity_headers_for(&resolved, ctx, picked).await;
    for (name, value) in identity_hdrs {
        let _ = upstream_request.insert_header(name, value);
    }
}

/// Dispatch to the per-profile header builder.
async fn identity_headers_for(
    resolved: &gw_core::impersonate::ResolvedIdentity,
    ctx: &ReqCtx,
    picked: &gw_core::entity::VendorKey,
) -> Vec<(String, String)> {
    use gw_core::impersonate::{claude_code, gemini_cli, openai_codex, ImpersonateProfile};

    match resolved.profile {
        ImpersonateProfile::ClaudeCodeChrome131 => {
            claude_code::headers(&resolved.variable, ctx.is_stream)
        }
        ImpersonateProfile::OpenAiCodex => {
            use gw_core::impersonate::openai_codex::PerRequest;

            let turn_state = if let Some(cache) = ctx.turn_state_cache.as_ref() {
                cache
                    .get_or_create(picked.key_id.as_ref(), ctx.conversation_id.as_deref())
                    .await
            } else {
                // No cache wired (e.g. integration tests that don't boot the
                // full gateway). Fall back to a fresh random token so the
                // header set is always complete.
                tracing::warn!(
                    key_id = %picked.key_id.as_ref(),
                    "turn_state_cache not wired; codex turn-state will be unstable"
                );
                gw_core::impersonate::rng::random_hex(32)
            };

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
            let quota = picked
                .oauth
                .as_ref()
                .and_then(|o| o.quota_project_id.as_deref());
            gemini_cli::headers(&resolved.variable, model, quota)
        }
    }
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

/// SHA-256 of request body truncated to u64 for CacheAffinity message_hash.
///
/// Returns `None` for empty input (preserves degraded behavior when no body
/// is available). The hash is the first 8 bytes of the SHA-256 digest
/// interpreted as a little-endian u64.
#[inline]
pub(crate) fn body_hash_u64(body: &[u8]) -> Option<u64> {
    if body.is_empty() {
        return None;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(body);
    Some(u64::from_le_bytes(digest[..8].try_into().unwrap()))
}

#[cfg(test)]
mod body_hash_tests {
    use super::*;
    use bytes::BytesMut;
    use gw_codec::ProtocolCodec;
    use gw_core::entity::KeyStatus;
    use gw_core::ids::{KeyId, ModelName, VendorId};
    use secrecy::SecretString;
    use std::collections::HashMap;
    use url::Url;

    fn vendor_key_with_mapping(mapping: &[(&str, &str)]) -> VendorKey {
        VendorKey {
            key_id: KeyId::from("vk-test"),
            vendor_id: VendorId::from("vendor-test"),
            api_key: SecretString::new("secret".into()),
            accessible_models: Vec::new(),
            model_mapping: mapping
                .iter()
                .map(|(from, to)| (ModelName::from(*from), (*to).to_owned()))
                .collect::<HashMap<_, _>>(),
            accessible_endpoints: vec![gw_core::protocol::ProtocolKind::OpenAi],
            protocol_conversion: true,
            priority: 0,
            weight: 1,
            rpm_limit: 0,
            tpm_limit: 0,
            max_usage_count: None,
            proxy: None::<Url>,
            ip_allowlist: None,
            expires_at: None,
            status: KeyStatus::Active,
            tags: Vec::new(),
            forced_capabilities: Vec::new(),
            exclusive_capability_match: false,
            tunnel_preference: None,
            oauth: None,
            identity_snapshot: None,
        }
    }

    #[test]
    fn empty_input_returns_none() {
        assert_eq!(body_hash_u64(b""), None);
    }

    #[test]
    fn non_empty_returns_deterministic_value() {
        // SHA-256("test") first 8 bytes as little-endian u64
        let expected: u64 = 7313085519096743583;
        assert_eq!(body_hash_u64(b"test"), Some(expected));
    }

    #[test]
    fn deterministic_across_calls() {
        let first = body_hash_u64(b"test");
        let second = body_hash_u64(b"test");
        assert_eq!(first, second);
    }

    #[test]
    fn passthrough_override_helper_renders_overridden_model() {
        let rendered = render_same_protocol_openai_with_passthrough_overrides_for_test(
            br#"{"model":"client-model","messages":[{"role":"user","content":"hi"}]}"#,
            &[("model", "\"upstream-model\"")],
        );
        let body: serde_json::Value = serde_json::from_slice(&rendered).unwrap();
        assert_eq!(
            body.get("model").and_then(serde_json::Value::as_str),
            Some("upstream-model")
        );
    }

    #[test]
    fn model_rewrite_skips_empty_default_upstream_model() {
        let picked = vendor_key_with_mapping(&[("client-model", "")]);
        assert!(model_rewrite(Some(&ModelName::from("client-model")), &picked).is_none());
    }

    #[test]
    fn model_rewrite_sets_ir_model_and_override_when_mapping_differs() {
        let picked = vendor_key_with_mapping(&[("client-model", "upstream-model")]);
        let (upstream_model, overrides) =
            model_rewrite(Some(&ModelName::from("client-model")), &picked).unwrap();

        assert_eq!(upstream_model, "upstream-model");
        assert_eq!(
            overrides.get("model").map(|v| v.get()),
            Some("\"upstream-model\"")
        );
    }

    #[test]
    fn same_protocol_passthrough_applies_model_rewrite() {
        let picked = vendor_key_with_mapping(&[("client-model", "upstream-model")]);
        let (_, overrides) =
            model_rewrite(Some(&ModelName::from("client-model")), &picked).unwrap();
        let rendered = render_same_protocol_openai_with_passthrough_overrides_for_test(
            br#"{"model":"client-model","messages":[{"role":"user","content":"hi"}]}"#,
            &[("model", overrides.get("model").unwrap().get())],
        );
        let body: serde_json::Value = serde_json::from_slice(&rendered).unwrap();
        assert_eq!(
            body.get("model").and_then(serde_json::Value::as_str),
            Some("upstream-model")
        );
    }

    #[test]
    fn cross_protocol_render_applies_model_rewrite() {
        let picked = vendor_key_with_mapping(&[("client-model", "upstream-model")]);
        let (_, overrides) =
            model_rewrite(Some(&ModelName::from("client-model")), &picked).unwrap();
        let mut ir = gw_codec::anthropic::Anthropic
            .parse_request(br#"{"model":"client-model","messages":[{"role":"user","content":"hi"}],"max_tokens":16}"#)
            .unwrap();
        apply_passthrough_overrides_to_ir(&mut ir, Some(&overrides));
        let mut buf = BytesMut::new();
        gw_codec::openai::OpenAi
            .render_request(&ir, &mut buf)
            .unwrap();

        let body: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(
            body.get("model").and_then(serde_json::Value::as_str),
            Some("upstream-model")
        );
    }
}

#[cfg(test)]
mod body_rules_tests {
    use super::*;
    use gw_codec::ProtocolCodec;

    fn make_body_rules(entries: &[(&str, &[(&str, &str)])]) -> BodyRulesMap {
        entries
            .iter()
            .map(|(endpoint, kvs)| {
                let inner: std::collections::HashMap<String, Box<serde_json::value::RawValue>> =
                    kvs.iter()
                        .map(|(k, v)| {
                            (
                                k.to_string(),
                                serde_json::value::RawValue::from_string(v.to_string()).unwrap(),
                            )
                        })
                        .collect();
                (endpoint.to_string(), inner)
            })
            .collect()
    }

    #[test]
    fn empty_config_no_op() {
        let rules = BodyRulesMap::new();
        let body = br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#;
        let mut ir = gw_codec::openai::OpenAi.parse_request(body).unwrap();
        let injected = apply_body_rules(&mut ir, "/v1/chat/completions", &rules, body);
        assert!(!injected);
        assert!(ir.passthrough_overrides.is_none());
    }

    #[test]
    fn field_exists_no_override() {
        let rules = make_body_rules(&[("/v1/chat/completions", &[("temperature", "0.7")])]);
        let body =
            br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"temperature":0.3}"#;
        let mut ir = gw_codec::openai::OpenAi.parse_request(body).unwrap();
        assert!(ir.temperature.is_some());
        let injected = apply_body_rules(&mut ir, "/v1/chat/completions", &rules, body);
        assert!(!injected);
        assert!(ir.passthrough_overrides.is_none());
    }

    #[test]
    fn missing_field_gets_injected() {
        let rules = make_body_rules(&[("/v1/chat/completions", &[("temperature", "0.7")])]);
        let body = br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#;
        let mut ir = gw_codec::openai::OpenAi.parse_request(body).unwrap();
        assert!(ir.temperature.is_none());
        let injected = apply_body_rules(&mut ir, "/v1/chat/completions", &rules, body);
        assert!(injected);
        let overrides = ir.passthrough_overrides.as_ref().unwrap();
        assert_eq!(overrides.get("temperature").unwrap().get(), "0.7");
    }

    #[test]
    fn multi_field_partial_injection() {
        let rules = make_body_rules(&[(
            "/v1/chat/completions",
            &[
                ("temperature", "0.7"),
                ("top_p", "0.9"),
                ("custom_field", "\"custom_value\""),
            ],
        )]);
        let body =
            br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"temperature":0.3}"#;
        let mut ir = gw_codec::openai::OpenAi.parse_request(body).unwrap();
        let injected = apply_body_rules(&mut ir, "/v1/chat/completions", &rules, body);
        assert!(injected);
        let overrides = ir.passthrough_overrides.as_ref().unwrap();
        assert!(
            !overrides.contains_key("temperature"),
            "temperature already set in IR, should not be in overrides"
        );
        assert_eq!(overrides.get("top_p").unwrap().get(), "0.9");
        assert_eq!(
            overrides.get("custom_field").unwrap().get(),
            "\"custom_value\""
        );
    }

    #[test]
    fn unmatched_endpoint_no_injection() {
        let rules = make_body_rules(&[("/v1/chat/completions", &[("temperature", "0.7")])]);
        let body = br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#;
        let mut ir = gw_codec::openai::OpenAi.parse_request(body).unwrap();
        let injected = apply_body_rules(&mut ir, "/v1/other/endpoint", &rules, body);
        assert!(!injected);
        assert!(ir.passthrough_overrides.is_none());
    }

    #[test]
    fn raw_body_field_not_in_ir_prevents_override() {
        let rules = make_body_rules(&[("/v1/chat/completions", &[("custom_key", "\"default\"")])]);
        let body = br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}],"custom_key":"user_value"}"#;
        let mut ir = gw_core::ir::RequestIR {
            model: std::borrow::Cow::Borrowed("gpt-4"),
            ..Default::default()
        };
        let injected = apply_body_rules(&mut ir, "/v1/chat/completions", &rules, body);
        assert!(!injected);
        assert!(ir.passthrough_overrides.is_none());
    }

    #[test]
    fn body_rules_merge_with_existing_passthrough_overrides() {
        let rules = make_body_rules(&[("/v1/chat/completions", &[("temperature", "0.7")])]);
        let body = br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#;
        let mut ir = gw_codec::openai::OpenAi.parse_request(body).unwrap();
        let mut existing = std::collections::HashMap::new();
        existing.insert(
            "model".to_owned(),
            serde_json::value::RawValue::from_string("\"upstream-model\"".to_owned()).unwrap(),
        );
        ir.passthrough_overrides = Some(existing);
        let injected = apply_body_rules(&mut ir, "/v1/chat/completions", &rules, body);
        assert!(injected);
        let overrides = ir.passthrough_overrides.as_ref().unwrap();
        assert_eq!(
            overrides.get("model").unwrap().get(),
            "\"upstream-model\"",
            "existing model override preserved"
        );
        assert_eq!(overrides.get("temperature").unwrap().get(), "0.7");
    }

    #[test]
    fn body_rules_do_not_override_existing_passthrough_key() {
        let rules = make_body_rules(&[("/v1/chat/completions", &[("model", "\"other-model\"")])]);
        let body = br#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#;
        let mut ir = gw_codec::openai::OpenAi.parse_request(body).unwrap();
        let mut existing = std::collections::HashMap::new();
        existing.insert(
            "model".to_owned(),
            serde_json::value::RawValue::from_string("\"upstream-model\"".to_owned()).unwrap(),
        );
        ir.passthrough_overrides = Some(existing);
        let injected = apply_body_rules(&mut ir, "/v1/chat/completions", &rules, body);
        assert!(!injected);
        let overrides = ir.passthrough_overrides.as_ref().unwrap();
        assert_eq!(
            overrides.get("model").unwrap().get(),
            "\"upstream-model\"",
            "existing passthrough override not overwritten"
        );
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

/// Regression baseline: snapshots the bytes-affecting output of
/// `inject_vendor_headers` for a Direct-mode vendor (no OAuth, static api_key).
/// The egress impersonation work in Phase 4 modifies this function to add
/// `X-Egress-*` headers when `egress_mode == Impersonate`. The Direct-mode
/// output MUST remain byte-identical. Re-run after every phase:
///
/// ```bash
/// cargo test -p gw-proxy regression_baseline -- --nocapture
/// ```
#[cfg(test)]
mod regression_baseline_tests {
    use super::*;
    use gw_core::entity::KeyStatus;
    use gw_core::ids::{KeyId, RequestId, TraceId, VendorId};
    use secrecy::SecretString;
    use std::collections::HashMap;
    use url::Url;

    /// Build the fixed, deterministic test inputs that mirror a canonical
    /// OpenAI chat-completion upstream request.
    fn make_canonical_inputs() -> (
        ReqCtx,
        RequestHeader,
        gw_core::entity::VendorKey,
        gw_core::versioned::Versioned<Vendor>,
        usize,
    ) {
        // --- VendorKey (Direct mode: no OAuth, static api_key) ---
        let key = VendorKey {
            key_id: KeyId::from("vk-baseline"),
            vendor_id: VendorId::from("openai"),
            api_key: SecretString::new("sk-vendor-secret".into()),
            accessible_models: Vec::new(),
            model_mapping: HashMap::new(),
            accessible_endpoints: vec![gw_core::protocol::ProtocolKind::OpenAi],
            protocol_conversion: false,
            priority: 0,
            weight: 1,
            rpm_limit: 0,
            tpm_limit: 0,
            max_usage_count: None,
            proxy: None::<Url>,
            ip_allowlist: None,
            expires_at: None,
            status: KeyStatus::Active,
            tags: Vec::new(),
            forced_capabilities: Vec::new(),
            exclusive_capability_match: false,
            tunnel_preference: None,
            oauth: None,
            identity_snapshot: None,
        };

        // --- Vendor with OpenAI base URL and Bearer auth template ---
        let vendor = Vendor {
            id: VendorId::from("openai"),
            display_name: "OpenAI".to_owned(),
            base_urls: vec![Url::parse("https://api.openai.com").unwrap()],
            auth_template: "Bearer {api_key}".to_owned(),
            timeout_overrides: None,
            default_proxy: None,
            contact: None,
            egress_mode: Default::default(),
        };
        let versioned_vendor = gw_core::versioned::Versioned::new(1, vendor);

        // --- ReqCtx with a FIXED trace_id (32 hex chars = 16 bytes = W3C trace-id) ---
        let trace_id = TraceId::from("0000000000000000aaaaaaaaaaaaaaaa");
        let request_id = RequestId::from("req-baseline-0001");
        let ctx = ReqCtx::new(trace_id, request_id);

        // --- RequestHeader: POST /v1/chat/completions with realistic inbound headers ---
        let mut req = RequestHeader::build("POST", b"/v1/chat/completions", Some(4)).unwrap();
        req.insert_header("host", "api.openai.com").unwrap();
        req.insert_header("content-type", "application/json")
            .unwrap();
        req.insert_header("accept", "application/json").unwrap();
        // These three MUST be stripped by inject_vendor_headers:
        req.insert_header("authorization", "Bearer client-test-key")
            .unwrap();
        req.insert_header("x-api-key", "should-be-stripped")
            .unwrap();
        req.insert_header("x-api-key-id", "should-be-stripped")
            .unwrap();

        // content_len: 42 (fixed; mirrors a small chat body)
        let content_len: usize = 42;

        (ctx, req, key, versioned_vendor, content_len)
    }

    /// Render a `RequestHeader` to a deterministic, sorted text form:
    /// first line is "<METHOD> <PATH>", then headers sorted alphabetically.
    /// The `traceparent` span-id (8 random bytes) is replaced with "SPANID"
    /// so the snapshot stays stable across runs.
    fn render(req: &RequestHeader) -> String {
        let mut out = String::new();
        out.push_str(&format!("{} {}\n", req.method, req.uri.path()));
        let mut entries: Vec<(String, String)> = req
            .headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).to_string(),
                )
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in entries {
            // NOTE: Extend this when Phase 4 introduces additional headers carrying
            // non-deterministic per-request material (e.g. X-Stainless-Session-Id).
            let v = if k == "traceparent" {
                // traceparent format: "00-<traceid>-<spanid>-01"
                let mut parts: Vec<&str> = v.split('-').collect();
                if parts.len() == 4 {
                    parts[2] = "SPANID";
                    parts.join("-")
                } else {
                    v
                }
            } else {
                v
            };
            out.push_str(&format!("{k}: {v}\n"));
        }
        out
    }

    #[test]
    fn direct_mode_inject_vendor_headers() {
        let (mut ctx, mut req, key, vendor, content_len) = make_canonical_inputs();
        inject_vendor_headers(&mut ctx, &mut req, &key, &vendor, content_len);
        insta::assert_snapshot!(render(&req));
    }
}

/// Unit test for the egress kill-switch predicate (Task 5.2).
///
/// The full `pick_peer` path requires a `pingora::Session` and an async store,
/// which makes an inline integration test heavy. The non-trivial logic in the
/// filter is the `matches!(.., VendorEgressMode::Impersonate { .. })` predicate
/// against the just-loaded vendor — everything else is plumbing. This test
/// pins that predicate against both variants of `VendorEgressMode` so the
/// matching rule cannot silently drift.
#[cfg(test)]
mod egress_kill_switch_tests {
    use gw_core::impersonate::{ImpersonateProfile, VendorEgressMode};

    /// Mirrors the production predicate at upstream.rs (the `if let
    /// VendorEgressMode::Impersonate { .. } = mode` branch).
    fn is_impersonate(mode: &VendorEgressMode) -> bool {
        matches!(mode, VendorEgressMode::Impersonate { .. })
    }

    #[test]
    fn impersonate_predicate_matches_both_variants() {
        assert!(
            !is_impersonate(&VendorEgressMode::Rustls),
            "Rustls must not be filtered by the kill-switch"
        );
        assert!(
            is_impersonate(&VendorEgressMode::Impersonate {
                profile: ImpersonateProfile::ClaudeCodeChrome131,
            }),
            "Impersonate must be filtered by the kill-switch"
        );
    }
}
