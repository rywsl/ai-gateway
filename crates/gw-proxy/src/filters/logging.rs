//! `logging` — final filter: release freeze, record key-health outcome, emit
//! UsageRecord + DetailedLog, optionally emit debug trace.

use crate::ctx::ReqCtx;
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use gw_health::{KeyHealthStore, OutcomeKind};
use gw_ledger::usage::{UsageRecord, UsageSource, UsageStatus};
use gw_ledger::{FreezeGuard, Ledger};
use gw_store::counter_store::daily_tokens_key;
use gw_store::CounterStore;
use pingora::ErrorType;
use pingora_proxy::Session;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;

/// Classify a completed request into one of 8 stable buckets:
/// `1xx`/`2xx`/`3xx`/`4xx`/`5xx`/`other`/`err`/`early`.
///
/// Short-circuit responses (auth/codec/quota) never set `upstream_status` because
/// the request never reached the upstream. `request_filter` returns an
/// `Err(pingora::Error::because(ErrorType::HTTPStatus(s), ...))` whose `etype`
/// carries the wire status we sent to the client. We extract that status so
/// `gateway_requests_total{status_class}` reports 4xx for auth rejections
/// rather than collapsing every short-circuit into the single `err` label.
pub fn status_class_for(
    err: Option<&pingora::Error>,
    upstream_status: Option<u16>,
) -> &'static str {
    if let Some(s) = upstream_status {
        return bucket(s);
    }
    if let Some(e) = err {
        if let ErrorType::HTTPStatus(s) = e.etype {
            return bucket(s);
        }
        return "err";
    }
    "early"
}

fn bucket(s: u16) -> &'static str {
    match s / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "other",
    }
}

pub async fn run(
    _session: &mut Session,
    err: Option<&pingora::Error>,
    ctx: &mut ReqCtx,
    health: &Arc<KeyHealthStore>,
    ledger: &Arc<Ledger>,
    counters: &Arc<CounterStore>,
) {
    let now = std::time::Instant::now();
    ctx.timing.end = Some(now);

    // Unconditional request counter. Pingora fires `logging` for every
    // completed request, including short-circuit responses from
    // `request_filter` (4xx auth/codec/rate-limit failures). The
    // UsageRecord/TPM emit path further down is gated on a 4-tuple of
    // `Some(..)` that is only populated once routing succeeds, so without
    // this counter a gateway that has only served 401s renders an empty
    // `/metrics` body. Emit early so any request hitting the proxy lights
    // up the exporter registry. `status_class_for` extracts the HTTPStatus
    // bucket from `pingora::Error::etype` when the short-circuit response
    // never set `upstream_status`, so 4xx auth rejections are no longer
    // collapsed into the single `err` label.
    let status_class = status_class_for(err, ctx.upstream_status);
    metrics::counter!(
        "gateway_requests_total",
        "status_class" => status_class,
    )
    .increment(1);

    // ---------------------------------------------------------------------
    // Release the freeze (release-only per C-γ §ADR-Cγ-2 — Rust no longer
    // computes cost; Go consumes usage:records and computes cost). Drop
    // would do the same if we forgot, but we release explicitly here so
    // the leak-metric stays clean.
    // ---------------------------------------------------------------------
    if let Some(g) = ctx.freeze_guard.as_ref() {
        g.release();
    }

    // ---------------------------------------------------------------------
    // Key-health bookkeeping. We treat a non-error status from upstream as
    // success, otherwise failure. The picked key's FormatHealth is fetched
    // on demand (multi-dim: key + api_format).
    // ---------------------------------------------------------------------
    let ok = err.is_none() && matches!(ctx.upstream_status, Some(s) if (200..400).contains(&s));
    if let Some(pk) = ctx.picked_vendor_key.as_ref() {
        // upstream_codec is set by upstream_request_filter (rewrite_request),
        // which runs after pick_peer. If rewrite_request fails before
        // committing the codec, picked_vendor_key is Some while
        // upstream_codec is None. The OpenAi fallback here is best-effort.
        //
        // Phase 3 K-B precondition: KeyHealthStore::record currently ignores
        // api_format in v1, but once K-B acts on per-format health state
        // (separate buckets per ApiFormat) a stale OpenAi fallback would
        // silently contaminate the wrong (key, OpenAi) bucket for a key that
        // actually served Anthropic traffic. Before K-B lands, a reliable
        // api_format source must be established at this call site (e.g.
        // capture it on ReqCtx during pick_peer).
        let api_fmt = ctx
            .upstream_codec
            .as_ref()
            .map(|c| c.kind())
            .unwrap_or(gw_core::protocol::ProtocolKind::OpenAi);
        let outcome = if ok {
            OutcomeKind::Success
        } else {
            OutcomeKind::Failure {
                http_status: ctx.upstream_status.unwrap_or(0),
                retry_after_secs: None,
            }
        };
        health.record(&pk.key_id, api_fmt, outcome);
    }

    // ---------------------------------------------------------------------
    // UsageRecord -> ledger.try_send.
    //
    // Payer semantics: Go consumes `usage:records` and debits the balance
    // counter, so `user_id` MUST be the *effective payer* — that is, the
    // parent account for a sub-account request, or the user itself
    // otherwise. The originating sub-account (when different) is recorded
    // in `child_id` for audit.
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
    if let (Some(auth_user_id), Some(effective), Some(model), Some(picked)) = (
        auth_user_id,
        effective_user_id.clone(),
        model.clone(),
        picked,
    ) {
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
        let rec = UsageRecord {
            ts_ms: now_ms,
            request_id: ctx.request_id.0.clone(),
            trace_id: ctx.trace_id.0.clone(),
            user_id: effective.clone(),
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
                UsageStatus::Ok
            } else {
                UsageStatus::Error
            },
            error_code: err.map(|e| format!("{:?}", e.etype)),
            http_status: ctx.upstream_status.unwrap_or(0),
            usage: UsageSnapshot {
                input_tokens: ctx.usage.input_tokens,
                output_tokens: ctx.usage.output_tokens,
                cache_read_input_tokens: ctx.usage.cached_tokens,
                cache_creation_input_tokens: ctx.usage.cache_creation_tokens,
                thinking_tokens: ctx.usage.thinking_tokens,
                multimodal_input_tokens: ctx.usage.multimodal_input_tokens,
            },
            usage_source: if ctx.usage.input_tokens > 0
                || ctx.usage.output_tokens > 0
                || ctx.usage.cached_tokens.unwrap_or(0) > 0
                || ctx.usage.cache_creation_tokens.unwrap_or(0) > 0
            {
                UsageSource::Upstream
            } else {
                UsageSource::Estimated
            },
            retry_count: ctx.retry_count,
            latency_ms,
            ttft_ms,
            is_stream: ctx.is_stream,
            // Use the trait method (the field is pub(crate) on RequestFreeze).
            freeze_amount_micro: ctx
                .freeze_guard
                .as_ref()
                .map(|g| g.freeze_amount())
                .unwrap_or(0),
            // Phase 5 (C-β) sources real version from entity:estimate_pricing.
            estimate_pricing_version: 0,
            markup_chain_snapshot: ctx
                .merged_perm
                .as_ref()
                .map(|m| m.markup_chain)
                .unwrap_or_else(MarkupChainSnapshot::placeholder),
            operation_id: ctx.operation_id.clone(),
        };

        // -----------------------------------------------------------------
        // W1-T7: Emit billing metrics at UsageRecord emission.
        // -----------------------------------------------------------------
        metrics::counter!(
            "gw_billing_emit_total",
            "usage_source" => format!("{:?}", rec.usage_source).to_lowercase(),
        )
        .increment(1);

        for (dim, present) in [
            ("input", ctx.usage.input_tokens > 0),
            ("output", ctx.usage.output_tokens > 0),
            ("cache_read", ctx.usage.cached_tokens.is_some()),
            ("cache_creation", ctx.usage.cache_creation_tokens.is_some()),
            ("thinking", ctx.usage.thinking_tokens.is_some()),
            (
                "multimodal_input",
                ctx.usage.multimodal_input_tokens.is_some(),
            ),
        ] {
            let tag = if present { "upstream_final" } else { "unknown" };
            metrics::counter!(
                "gw_billing_provenance_total",
                "dimension" => dim,
                "tag" => tag,
            )
            .increment(1);
        }

        let _ = ledger.try_send(rec);

        // -----------------------------------------------------------------
        // G3 (ADR-G3-2) — daily-quota post-increment. Bump
        // `counter:user:{effective_user_id}:daily_tokens:{YYYY-MM-DD}` by
        // the observed input+output tokens and pin a 48 h TTL so old days
        // age out without an explicit sweeper. Best-effort: Redis errors
        // are logged but never propagate (daily quota is observational,
        // not billing).
        //
        // TODO(metrics): wire `gw_daily_quota_increment_error_total` once
        // the metrics catalog opens a slot for it (out of scope for T2-A —
        // see the T2-A plan note about metrics_catalog gating).
        // -----------------------------------------------------------------
        let total_tokens =
            (ctx.usage.input_tokens as u64).saturating_add(ctx.usage.output_tokens as u64);
        if total_tokens > 0 {
            let d = OffsetDateTime::now_utc().date();
            let day = format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day());
            let key = daily_tokens_key(&effective, &day);
            let counters_for_spawn = counters.clone();
            // Best-effort fire-and-forget; the request lifecycle does not
            // wait for this to complete. 48 h TTL = 172800 s.
            tokio::spawn(async move {
                if let Err(e) = counters_for_spawn
                    .incrby_with_expire(&key, total_tokens as i64, 172_800)
                    .await
                {
                    tracing::warn!(target: "gw_daily_quota", error = %e, "daily quota increment failed");
                }
            });
        }

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

    // ---------------------------------------------------------------------
    // T6-B: invoke ErrorHandler fanout if a ClassifierOutcome was produced
    // earlier in the filter chain. Listeners are stateless stubs in v1 so
    // ad-hoc construction is acceptable; the follow-up moves construction to
    // `LlmGateway::new` so shared Arc handles (KeyHealthStore, EntityStore,
    // ResilienceState) can be threaded through `HandlerCtx`.
    // ---------------------------------------------------------------------
    if let Some(oc) = ctx.error_outcome.as_ref() {
        use crate::error::handler::{ErrorHandler, HandlerCtx};
        use crate::error::listeners::{
            cache_invalidate::CacheInvalidateListener, key_health::KeyHealthListener,
            ledger::LedgerListener, metrics::MetricsListener, oauth::OAuthListener,
            rate_limit::RateLimitFeedbackListener,
        };
        #[allow(clippy::default_constructed_unit_structs)]
        let handler = ErrorHandler::new(vec![
            Arc::new(KeyHealthListener::default()) as _,
            Arc::new(RateLimitFeedbackListener::default()) as _,
            Arc::new(OAuthListener::default()) as _,
            Arc::new(CacheInvalidateListener::default()) as _,
            Arc::new(MetricsListener::default()) as _,
            Arc::new(LedgerListener::default()) as _,
        ]);
        // TODO(T6-B follow-up): populate real fields (request_id, api_key_id,
        // vendor_key_id, api_format, key_health, entity_store, resilience).
        let hctx = HandlerCtx::stub();
        handler.fanout(&hctx, oc);
    }
}
