//! `logging` — final filter: release freeze, record key-health outcome, emit
//! UsageRecord + DetailedLog, optionally emit debug trace.

use crate::ctx::ReqCtx;
use crate::detailed_log::{redacted_preview, DetailedLogRecord};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::traits::Redactor;
use gw_core::usage::UsageSnapshot;
use gw_health::{KeyHealthStore, OutcomeKind};
use gw_ledger::usage::{UsageRecord, UsageSource, UsageStatus};
use gw_ledger::{FreezeGuard, Ledger};
use pingora::ErrorType;
use pingora_proxy::Session;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
    health: &Arc<KeyHealthStore>,
    ledger: &Arc<Ledger>,
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
                // Phase 2 (codec S4 extract_usage_any) populates the
                // remaining three fields directly from upstream wire.
                cache_creation_input_tokens: None,
                thinking_tokens: None,
                multimodal_input_tokens: None,
            },
            usage_source: if ctx.usage.output_tokens > 0 {
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
