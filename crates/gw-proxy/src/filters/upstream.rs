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
use crate::timeouts::Timeouts;
use bytes::BytesMut;
use gw_codec::passthrough::same_protocol_passthrough;
use gw_codec::AnyCodec;
use gw_core::entity::{ModelGroup, StrategyKind};
use gw_core::ir::Mutations;
use gw_routing::candidates::get_candidate_keys;
use gw_routing::strategy::{
    cache_affinity::CacheAffinity, cost_first::CostFirst, fixed_order::FixedOrder,
    latency_first::LatencyFirst, load_balance::LoadBalance, round_robin::RoundRobin,
    sticky::Sticky, weighted_rr::WeightedRr,
};
use gw_routing::{BreakerStore, InFlightGuard, PickCtx, Strategy, StrategyAny};
use gw_store::EntityStore;
use pingora::http::RequestHeader;
use pingora::upstreams::peer::HttpPeer;
use pingora::{Error, ErrorType, Result};
use pingora_proxy::Session;
use secrecy::ExposeSecret;
use std::sync::Arc;

/// Per-key in-flight cap used by the candidate filter. v1: 1024; v2 will move
/// this into config.
const PER_KEY_INFLIGHT_CAP: u32 = 1024;

pub async fn pick_peer(
    _session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    breakers: &Arc<BreakerStore>,
) -> Result<Box<HttpPeer>> {
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
    let group_id = perm
        .forced_model_group
        .clone()
        .or_else(|| perm.allowed_groups.iter().next().cloned())
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_no_model_group")))?;

    let group: Arc<gw_core::versioned::Versioned<ModelGroup>> = store
        .model_group
        .get(&group_id)
        .await
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_model_group_missing")))?;
    // Lazy load pricing too (Phase-4 v1: not yet used in routing decisions but
    // cached on ctx for the logging filter).
    if let Some(p) = store.pricing.get(&model).await {
        ctx.pricing = Some(p);
    }

    // -------------------------------------------------------------
    // Candidate filter.
    // -------------------------------------------------------------
    let candidates = get_candidate_keys(
        &group.value,
        &model,
        client_codec_kind,
        &perm,
        breakers,
        store,
        PER_KEY_INFLIGHT_CAP,
    )
    .await;
    if candidates.is_empty() {
        return Err(Error::new(ErrorType::Custom(
            "gw_routing_no_available_channel",
        )));
    }

    // -------------------------------------------------------------
    // Strategy dispatch.
    // -------------------------------------------------------------
    let strategy = strategy_for(group.value.strategy);
    let pick_ctx = PickCtx {
        user_id: &perm.effective_user_id,
        model: &model,
        conversation_id: None,
        message_hash: None,
    };
    let picked = strategy
        .pick(&candidates, breakers, &pick_ctx)
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_routing_strategy_empty")))?;
    let picked_arc = Arc::clone(picked);
    let vendor = store
        .vendor
        .get(&picked_arc.vendor_id)
        .await
        .ok_or_else(|| Error::new(ErrorType::Custom("gw_vendor_missing")))?;

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
    let mut peer = HttpPeer::new(format!("{host}:{port}"), scheme_tls, host);

    // -------------------------------------------------------------
    // Timeouts. Pricing-supplied overrides take precedence over the
    // generic defaults.
    // -------------------------------------------------------------
    let mut t = Timeouts::default_for_nonstream();
    if let Some(p) = ctx.pricing.as_ref() {
        if let Some(ms) = p.value.connect_timeout_ms {
            t.connect = std::time::Duration::from_millis(ms as u64);
        }
        if let Some(ms) = p.value.read_timeout_ms {
            t.read = std::time::Duration::from_millis(ms as u64);
        }
        if let Some(ms) = p.value.total_timeout_ms {
            t.total = std::time::Duration::from_millis(ms as u64);
        }
        if let Some(ms) = p.value.stream_total_timeout_ms {
            t.stream_total = std::time::Duration::from_millis(ms as u64);
        }
    }
    t.apply_to_peer(&mut peer, ctx.is_stream);

    // Stash on ctx for upstream_request_filter + logging.
    ctx.picked_vendor_key = Some(picked_arc.clone());
    ctx.picked_vendor = Some(vendor);

    // Install the in-flight RAII guard now that we've committed to a key.
    // The Drop impl decrements the counter on every termination path
    // (success, error, cancel) — see `gw_routing::InFlightGuard`. This is
    // what makes `LoadBalance::pick` and `get_candidate_keys`'s per-key
    // in-flight cap (PER_KEY_INFLIGHT_CAP = 1024) actually observe traffic.
    let breaker = breakers.get_or_create(&picked_arc.key_id);
    ctx.inflight_guard = Some(InFlightGuard::new(breaker));

    Ok(Box::new(peer))
}

fn strategy_for(kind: StrategyKind) -> StrategyAny {
    match kind {
        StrategyKind::FixedOrder => StrategyAny::FixedOrder(FixedOrder),
        StrategyKind::RoundRobin => StrategyAny::RoundRobin(RoundRobin::default()),
        StrategyKind::LoadBalance => StrategyAny::LoadBalance(LoadBalance),
        StrategyKind::WeightedRr => StrategyAny::WeightedRr(WeightedRr::default()),
        StrategyKind::CostFirst => StrategyAny::CostFirst(CostFirst),
        StrategyKind::LatencyFirst => StrategyAny::LatencyFirst(LatencyFirst),
        StrategyKind::CacheAffinity => StrategyAny::CacheAffinity(CacheAffinity),
        StrategyKind::Sticky => StrategyAny::Sticky(Sticky::default()),
    }
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

    // Headers: rewrite Authorization per the vendor's auth_template, drop
    // the inbound auth headers, set Host, propagate traceparent.
    let _ = upstream_request.remove_header("authorization");
    let _ = upstream_request.remove_header("x-api-key");
    let _ = upstream_request.remove_header("x-api-key-id");

    let api_key = picked.api_key.expose_secret().to_owned();
    let auth_value = vendor.value.auth_template.replace("{api_key}", &api_key);
    let _ = upstream_request.insert_header("authorization", auth_value);
    if let Some(host) = vendor
        .value
        .base_urls
        .first()
        .and_then(|u| u.host_str())
    {
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
