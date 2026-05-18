//! `logging` — final filter: settle freeze, record breaker outcome, emit
//! BillingRecord + DetailedLog, optionally emit debug trace.

use crate::ctx::ReqCtx;
use crate::detailed_log::{redacted_preview, DetailedLogRecord};
use gw_core::traits::Redactor;
use gw_ledger::billing::{BillingRecord, BillingStatus, UsageSource};
use gw_ledger::Ledger;
use gw_routing::BreakerStore;
use pingora_proxy::Session;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Placeholder redactor used until a config-driven implementation lands.
/// Performs no redaction; the preview is truncated to `PREVIEW_MAX_BYTES` by
/// [`redacted_preview`].
struct NoopRedactor;
impl Redactor for NoopRedactor {
    fn redact(&self, _s: &mut String) {}
}

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
    //
    // Payer semantics: the flusher debits `counter:user:{user_id}:balance_micro`,
    // so `user_id` MUST be the *effective payer* — that is, the parent account
    // for a sub-account request, or the user itself otherwise. The originating
    // sub-account (when different) is recorded in `child_id` for audit.
    // ---------------------------------------------------------------------
    let auth_user_id = ctx
        .merged_perm
        .as_ref()
        .map(|m| m.user_id.clone())
        .or_else(|| ctx.user.as_ref().map(|u| u.value.user_id.clone()));
    let effective_user_id = ctx
        .merged_perm
        .as_ref()
        .map(|m| m.effective_user_id.clone())
        .or_else(|| auth_user_id.clone());
    let model = ctx.model.clone();
    let picked = ctx.picked_vendor_key.as_ref();
    if let (Some(auth_user_id), Some(effective), Some(model), Some(picked)) =
        (auth_user_id, effective_user_id.clone(), model.clone(), picked)
    {
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
        let is_subaccount = effective != auth_user_id;
        let rec = BillingRecord {
            ts_ms: now_ms,
            request_id: ctx.request_id.0.clone(),
            trace_id: ctx.trace_id.0.clone(),
            // PAYER — flusher debits this user's balance counter.
            user_id: effective.clone(),
            // Request originator when it differs from the payer (sub-account).
            child_id: if is_subaccount {
                Some(auth_user_id.clone())
            } else {
                None
            },
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
            model: model.clone(),
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

        // -----------------------------------------------------------------
        // DetailedLogRecord — emitted via tracing for v1. A dedicated sink
        // in gw-ledger will replace this once the wiring lands.
        // Body previews are gated on debug.include_*_body so we don't pay
        // the redactor cost or leak content on the normal path.
        // -----------------------------------------------------------------
        let redactor = NoopRedactor;
        let request_body_preview = if ctx.debug.include_request_body {
            // ctx.cached_body holds the body we sent to upstream (same as the
            // original downstream body for same-protocol passthrough).
            ctx.cached_body
                .as_ref()
                .and_then(|b| redacted_preview(b, &redactor))
                .map(|s| truncate_to(s, 512))
        } else {
            None
        };
        // v1: response body preview is not currently captured on ctx; the
        // SSE/unary response-body filter would need to buffer it. Leave None
        // until that wiring lands.
        let response_body_preview: Option<String> = None;
        let _ = ctx.debug.include_response_body; // suppress dead-field warning

        let detailed = DetailedLogRecord {
            ts_ms: now_ms,
            trace_id: ctx.trace_id.clone(),
            request_id: ctx.request_id.clone(),
            user_id: Some(effective.clone()),
            api_key_id: ctx.api_key_id.clone(),
            model: Some(model),
            client_protocol: ctx
                .client_codec
                .as_ref()
                .map(|c| c.kind().as_str().to_owned()),
            upstream_protocol: ctx
                .upstream_codec
                .as_ref()
                .map(|c| c.kind().as_str().to_owned()),
            http_status: ctx.upstream_status.unwrap_or(0),
            error_code: err.map(|e| format!("{:?}", e.etype)),
            is_stream: ctx.is_stream,
            input_tokens: ctx.usage.input_tokens,
            output_tokens: ctx.usage.output_tokens,
            latency_ms,
            ttft_ms,
            request_body_preview,
            response_body_preview,
        };
        tracing::info!(
            target: "gw_detailed_log",
            trace_id = %ctx.trace_id,
            request_id = %ctx.request_id,
            record = ?detailed,
            "detailed log"
        );

        // -----------------------------------------------------------------
        // TPM consumption metric. Spec step 3 expects this to feed back into
        // a RateLimitGuard TPM tracker; gw-auth doesn't expose that API yet
        // (see TODO in gw-auth/src/ratelimit.rs), so for now we emit a
        // counter that downstream observability can scrape.
        // -----------------------------------------------------------------
        let tpm_tokens = (ctx.usage.input_tokens as u64) + (ctx.usage.output_tokens as u64);
        if tpm_tokens > 0 {
            metrics::counter!(
                "gateway_tpm_consumed_total",
                "user_id" => effective.to_string(),
            )
            .increment(tpm_tokens);
        }
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

/// Truncate `s` to at most `max_chars` characters, respecting UTF-8 boundaries.
/// Appends a short marker when truncation occurs.
fn truncate_to(mut s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s;
    }
    let cut = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s.truncate(cut);
    s.push_str("…(truncated)");
    s
}
