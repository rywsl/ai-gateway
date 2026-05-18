//! `logging` — final filter: settle freeze, record breaker outcome, emit
//! BillingRecord + DetailedLog, optionally emit debug trace.

use crate::ctx::ReqCtx;
use gw_ledger::billing::{BillingRecord, BillingStatus, UsageSource};
use gw_ledger::Ledger;
use gw_routing::BreakerStore;
use pingora_proxy::Session;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub async fn run(
    _session: &mut Session,
    err: Option<&pingora::Error>,
    ctx: &mut ReqCtx,
    breakers: &Arc<BreakerStore>,
    ledger: &Arc<Ledger>,
) {
    let now = std::time::Instant::now();
    ctx.timing.end = Some(now);

    // ---------------------------------------------------------------------
    // Compute the real cost (micro) using ctx.usage + (optional) pricing.
    // The freeze guard, if present, is settled with this number. If there's
    // no pricing we charge nothing — gw-proxy defers conservative-fallback
    // accounting to gw-ledger's flusher.
    // ---------------------------------------------------------------------
    let cost_micro = if let Some(p) = ctx.pricing.as_ref() {
        let in_cost = ctx.usage.input_tokens as u64 * p.value.input_price_micro_per_token;
        let out_cost = ctx.usage.output_tokens as u64 * p.value.output_price_micro_per_token;
        let raw = in_cost + out_cost;
        let markup = ctx
            .merged_perm
            .as_ref()
            .map(|m| m.markup_product.max(1.0))
            .unwrap_or(1.0);
        ((raw as f32) * markup) as u64
    } else {
        0
    };
    if let Some(g) = ctx.freeze_guard.as_ref() {
        g.settle(cost_micro);
    }

    // ---------------------------------------------------------------------
    // Breaker bookkeeping. We treat a non-error status from upstream as
    // success, otherwise failure. The picked key's BreakerState is fetched
    // on demand.
    // ---------------------------------------------------------------------
    let ok = err.is_none() && matches!(ctx.upstream_status, Some(s) if (200..400).contains(&s));
    if let Some(pk) = ctx.picked_vendor_key.as_ref() {
        breakers.get_or_create(&pk.key_id).record(ok);
    }

    // ---------------------------------------------------------------------
    // BillingRecord -> ledger.try_send.
    // ---------------------------------------------------------------------
    let user_id = ctx
        .merged_perm
        .as_ref()
        .map(|m| m.user_id.clone())
        .or_else(|| ctx.user.as_ref().map(|u| u.value.user_id.clone()));
    let model = ctx.model.clone();
    let picked = ctx.picked_vendor_key.as_ref();
    if let (Some(user_id), Some(model), Some(picked)) = (user_id, model, picked) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let latency_ms = match (ctx.timing.start, ctx.timing.end) {
            (Some(s), Some(e)) => e.saturating_duration_since(s).as_millis() as u32,
            _ => 0,
        };
        let ttft_ms = match (ctx.timing.start, ctx.timing.ttft) {
            (Some(s), Some(t)) => Some(t.saturating_duration_since(s).as_millis() as u32),
            _ => None,
        };
        let rec = BillingRecord {
            ts_ms: now_ms,
            request_id: ctx.request_id.0.clone(),
            trace_id: ctx.trace_id.0.clone(),
            user_id,
            child_id: ctx.user.as_ref().and_then(|u| u.value.parent_id.clone().map(|_| u.value.user_id.clone())),
            api_key_id: ctx.api_key_id.clone(),
            client_protocol: ctx
                .client_codec
                .as_ref()
                .map(|c| c.kind().as_str().to_owned())
                .unwrap_or_default(),
            upstream_protocol: ctx
                .upstream_codec
                .as_ref()
                .map(|c| c.kind().as_str().to_owned())
                .unwrap_or_default(),
            model,
            vendor_id: picked.vendor_id.clone(),
            vendor_key_id: picked.key_id.clone(),
            status: if ok {
                BillingStatus::Ok
            } else {
                BillingStatus::Error
            },
            error_code: err.map(|e| format!("{:?}", e.etype)),
            http_status: ctx.upstream_status.unwrap_or(0),
            input_tokens: ctx.usage.input_tokens,
            output_tokens: ctx.usage.output_tokens,
            cached_tokens: ctx.usage.cached_tokens,
            cost_micro,
            freeze_amount_micro: ctx
                .freeze_guard
                .as_ref()
                .map(|g| g.freeze_amount)
                .unwrap_or(0),
            markup_product: ctx
                .merged_perm
                .as_ref()
                .map(|m| m.markup_product)
                .unwrap_or(1.0),
            usage_source: if ctx.usage.output_tokens > 0 {
                UsageSource::Upstream
            } else {
                UsageSource::Estimated
            },
            retry_count: ctx.retry_count,
            latency_ms,
            ttft_ms,
            is_stream: ctx.is_stream,
        };
        let _ = ledger.try_send(rec);
    }

    if ctx.debug.enabled {
        tracing::info!(
            target: "gw_debug",
            trace_id = %ctx.trace_id,
            request_id = %ctx.request_id,
            phases = ctx.trace.phase_count,
            "debug trace buffer emitted"
        );
    }
}
