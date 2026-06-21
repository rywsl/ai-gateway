//! `logging` — final filter: release freeze, record key-health outcome, emit
//! UsageRecord + DetailedLog, optionally emit debug trace.

use crate::ctx::ReqCtx;
use gw_auth::ratelimit::RateLimitGuard;
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::ProvenanceTag;
use gw_core::usage::UsageSnapshot;
use gw_health::{KeyHealthStore, OutcomeKind};
use gw_ledger::usage::{UsageProvenance, UsageRecord, UsageStatus, VendorExposureState};
use gw_ledger::{ClearKey, FreezeGuard, Ledger, LedgerItem};
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

#[allow(clippy::too_many_arguments)]
pub async fn run(
    _session: &mut Session,
    err: Option<&pingora::Error>,
    ctx: &mut ReqCtx,
    health: &Arc<KeyHealthStore>,
    ledger: &Arc<Ledger>,
    counters: &Arc<CounterStore>,
    store: &Arc<gw_store::EntityStore>,
    resilience: &crate::resilience::ResilienceState,
    affinity: &gw_routing::affinity::AffinityManager,
    ratelimit: &Arc<RateLimitGuard>,
    quota_state_updates: Option<&Arc<gw_store::quota_state::QuotaStateUpdateQueue>>,
    overflow_gate: &Arc<gw_ledger::overflow::OverflowGate>,
) {
    // W2-T4: force Terminated on any request that reached Sent or Streaming
    // but didn't arrive at a terminal state (cancel, error, deadline, etc.).
    // Idle requests (never dispatched to upstream) are left alone.
    {
        use crate::billing_lifecycle::{
            is_terminal as exposure_is_terminal, transition_to, VendorExposure,
        };
        if matches!(
            ctx.vendor_exposure,
            VendorExposure::Sent | VendorExposure::Streaming
        ) {
            transition_to(ctx, VendorExposure::Terminated);
        }
        let _ = exposure_is_terminal; // silence unused-import warning
    }

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
    // Credit-lease settle (protected mode). Compute actual credits consumed
    // using the SAME coef_table_version the lease was acquired under, then
    // refund the difference if actual ≤ reserved. Spec 2026-05-29 §6.4.
    //
    // The two cases are mutually exclusive (request_filter step 8.5 only
    // sets lease_guard on success and bypasses step 10's RequestFreeze).
    // The settle MUST happen before the UsageRecord literal below reads
    // `ctx.credits_consumed` / `ctx.coef_table_version_at_settle`.
    // ---------------------------------------------------------------------
    let ok = err.is_none() && matches!(ctx.upstream_status, Some(s) if (200..400).contains(&s));

    if let Some(guard) = ctx.lease_guard.as_ref() {
        let coef_version = guard.coef_version();
        let model_name = ctx
            .model
            .as_ref()
            .map(|m| m.as_ref().to_string())
            .unwrap_or_default();
        let Some((usage_snap, _)) = usage_snapshot_and_provenance_for_record(ctx, ok) else {
            tracing::error!(
                target: "gw_proxy::filters::logging",
                request_id = %ctx.request_id,
                trace_id = %ctx.trace_id,
                client_shape = ?ctx.client_shape,
                vendor_exposure = ?ctx.vendor_exposure,
                "skipping credit-lease settle due to CLI billing protection"
            );
            if let Some(g) = ctx.lease_guard.as_ref() {
                g.release();
            }
            return;
        };
        match gw_ledger::actual_usage_credits(coef_version, model_name.as_str(), &usage_snap) {
            Ok(actual) => {
                if actual > guard.reserved() {
                    metrics::counter!(
                        gw_ledger::metrics::CREDIT_ACTUAL_EXCEEDS_RESERVED_TOTAL,
                        "model_id" => model_name.clone(),
                    )
                    .increment(1);
                }
                guard.settle_actual(actual);
                if let Err(err) = overflow_gate
                    .record_actual(guard.uid().as_ref(), guard.reserved(), actual)
                    .await
                {
                    tracing::warn!(
                        target: "gw_proxy::filters::logging",
                        user_id = %guard.uid(),
                        request_id = %ctx.request_id,
                        error = ?err,
                        "credit-lease overflow record_actual failed"
                    );
                }
                ctx.credits_consumed = Some(actual);
                ctx.coef_table_version_at_settle = Some(coef_version);
            }
            Err(e) => {
                tracing::warn!(
                    target: "gw_proxy::filters::logging",
                    error = ?e,
                    coef_version,
                    model = %model_name,
                    "credit-lease actual_usage_credits failed; settle skipped"
                );
            }
        }
    }

    if let Some(g) = ctx.lease_guard.as_ref() {
        g.release();
    }

    // ---------------------------------------------------------------------
    // Key-health bookkeeping. We treat a non-error status from upstream as
    // success, otherwise failure. The picked key's FormatHealth is fetched
    // on demand (multi-dim: key + api_format).
    // ---------------------------------------------------------------------
    if let Some(pk) = ctx.picked_vendor_key.as_ref() {
        // Audit 2026-05-30 §1.5: record into the SAME (key, ApiFormat) bucket the
        // routing health gate read. `record()` keys by api_format, so the bucket
        // must match selection. Normally `upstream_codec` is already the resolved
        // upstream format (set by rewrite_request via the shared resolver). If
        // rewrite_request failed before committing the codec, derive the bucket
        // identically — resolve_upstream_format(accessible, client) — instead of a
        // hardcoded OpenAi fallback that would contaminate the wrong bucket for a
        // protocol-conversion key.
        let api_fmt = match ctx.upstream_codec.as_ref() {
            Some(c) => c.kind(),
            None => {
                let client_kind = ctx
                    .client_codec
                    .as_ref()
                    .map(|c| c.kind())
                    .unwrap_or(gw_core::protocol::ProtocolKind::OpenAi);
                gw_core::protocol::resolve_upstream_format(&pk.accessible_endpoints, client_kind)
            }
        };
        let retry_after_secs = ctx
            .error_outcome
            .as_ref()
            .and_then(|outcome| outcome.rate_limit_hint.as_ref())
            .and_then(|hint| hint.retry_after_secs);
        let outcome = if ok {
            OutcomeKind::Success
        } else {
            OutcomeKind::Failure {
                http_status: ctx.upstream_status.unwrap_or(0),
                retry_after_secs,
            }
        };
        health.record(&pk.key_id, api_fmt, outcome);
        if let (Some(queue), Some(snapshot)) = (quota_state_updates, ctx.quota_snapshot.as_ref()) {
            queue.schedule(pk.key_id.0.clone(), snapshot.state.clone());
        }
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
        let Some(rec) =
            build_final_usage_record(ctx, ok, err, &auth_user_id, &effective, &model, picked)
        else {
            tracing::error!(
                target: "gw_proxy::filters::logging",
                request_id = %ctx.request_id,
                trace_id = %ctx.trace_id,
                client_shape = ?ctx.client_shape,
                vendor_exposure = ?ctx.vendor_exposure,
                "skipping final usage record emission due to CLI billing protection"
            );
            return;
        };

        // -----------------------------------------------------------------
        // W1-T7: Emit billing metrics at UsageRecord emission.
        // -----------------------------------------------------------------
        metrics::counter!(
            "gw_billing_emit_total",
            "vendor_exposure_state" => format!("{:?}", rec.vendor_exposure_state).to_lowercase(),
        )
        .increment(1);

        for (dim, provenance, present) in [
            (
                "input",
                rec.usage_provenance.input,
                rec.usage.input_tokens > 0,
            ),
            (
                "output",
                rec.usage_provenance.output,
                rec.usage.output_tokens > 0,
            ),
            (
                "cache_read",
                rec.usage_provenance.cache_read,
                rec.usage.cache_read_input_tokens.is_some(),
            ),
            (
                "cache_creation",
                rec.usage_provenance.cache_creation,
                rec.usage.cache_creation_input_tokens.is_some(),
            ),
            (
                "thinking",
                rec.usage_provenance.thinking,
                rec.usage.thinking_tokens.is_some(),
            ),
            (
                "multimodal_input",
                rec.usage_provenance.multimodal_input,
                rec.usage.multimodal_input_tokens.is_some(),
            ),
        ] {
            let tag = if present {
                provenance_label(provenance)
            } else {
                "unknown"
            };
            metrics::counter!(
                "gw_billing_provenance_total",
                "dimension" => dim,
                "tag" => tag,
            )
            .increment(1);
        }

        let _ = ledger
            .emit(LedgerItem {
                record: rec,
                clear: Some(ClearKey {
                    request_id: ctx.request_id.0.clone(),
                    attempt_index: ctx.attempt_index,
                }),
            })
            .await;

        // -----------------------------------------------------------------
        // G3 (ADR-G3-2) — daily-quota post-increment. Bump
        // `counter:user:{effective_user_id}:daily_tokens:{YYYY-MM-DD}` by
        // the observed input+output tokens and pin a 48 h TTL so old days
        // age out without an explicit sweeper. Best-effort: Redis errors
        // are logged but never propagate (daily quota is observational,
        // not billing).
        // -----------------------------------------------------------------
        let total_tokens =
            (ctx.usage.input_tokens as u64).saturating_add(ctx.usage.output_tokens as u64);
        if total_tokens > 0 {
            let d = OffsetDateTime::now_utc().date();
            let day = format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day());
            let key = daily_tokens_key(&effective, &day);
            let counters_for_spawn = counters.clone();
            tokio::spawn(async move {
                if let Err(e) = counters_for_spawn
                    .incrby_with_expire(&key, total_tokens as i64, 172_800)
                    .await
                {
                    tracing::warn!(target: "gw_daily_quota", error = %e, "daily quota increment failed");
                    metrics::counter!("gw_daily_quota_increment_error_total").increment(1);
                }
            });
        }

        // -----------------------------------------------------------------
        // TPM deferred deduction (T2). Feeds observed tokens into the
        // per-user sliding-window TPM limiter. Fire-and-forget: the
        // response is already sent, so a breach only gates the NEXT request.
        // -----------------------------------------------------------------
        let tpm_tokens = (ctx.usage.input_tokens as u64) + (ctx.usage.output_tokens as u64);
        if tpm_tokens > 0 {
            metrics::counter!("gateway_tpm_consumed_total").increment(tpm_tokens);

            if let Some(perm) = ctx.merged_perm.as_ref() {
                if let Err(e) = ratelimit.deduct_tpm(&effective, perm, tpm_tokens as u32) {
                    tracing::info!(
                        target: "gw_tpm",
                        user_id = %effective,
                        tokens = tpm_tokens,
                        code = %e.code,
                        "tpm budget exhausted; next request will be rejected"
                    );
                }
            }
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

    // T6-B: invoke ErrorHandler fanout if a ClassifierOutcome was produced
    // earlier in the filter chain.
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
        let hctx = HandlerCtx {
            request_id: ctx.request_id.0.as_str(),
            api_key_id: ctx.api_key_id.as_ref().map(|id| id.0.as_str()),
            vendor_key_id: ctx.picked_vendor_key.as_ref().map(|k| k.key_id.0.as_str()),
            api_format: ctx.upstream_codec.as_ref().map(|c| c.kind()),
            key_health: Some(&**health),
            entity_store: Some(store),
            resilience: Some(resilience),
            affinity: Some(affinity),
        };
        handler.fanout(&hctx, oc);
    }
}

fn provenance_label(tag: ProvenanceTag) -> &'static str {
    match tag {
        ProvenanceTag::UpstreamFinal => "upstream_final",
        ProvenanceTag::CliExtracted => "cli_extracted",
        ProvenanceTag::UpstreamPartial => "upstream_partial",
        ProvenanceTag::Estimated => "estimated",
        ProvenanceTag::CrashFallback => "crash_fallback",
        ProvenanceTag::Unknown => "unknown",
    }
}

fn raw_usage_snapshot(ctx: &ReqCtx) -> UsageSnapshot {
    UsageSnapshot {
        input_tokens: ctx.usage.input_tokens,
        output_tokens: ctx.usage.output_tokens,
        cache_read_input_tokens: ctx.usage.cached_tokens,
        cache_creation_input_tokens: ctx.usage.cache_creation_tokens,
        thinking_tokens: ctx.usage.thinking_tokens,
        multimodal_input_tokens: ctx.usage.multimodal_input_tokens,
        audio_input_tokens: None,
        audio_output_tokens: None,
        accepted_prediction_tokens: None,
        rejected_prediction_tokens: None,
        cache_creation_5m_tokens: None,
        cache_creation_1h_tokens: None,
        server_tool_web_search_count: None,
        server_tool_web_fetch_count: None,
        pv: gw_core::usage::CURRENT_PV,
    }
}

fn usage_snapshot_and_provenance_for_record(
    ctx: &ReqCtx,
    ok: bool,
) -> Option<(UsageSnapshot, UsageProvenance)> {
    if ok {
        crate::billing_lifecycle::billing_protected_usage(ctx)
    } else {
        Some((raw_usage_snapshot(ctx), ctx.usage_provenance))
    }
}

fn build_final_usage_record(
    ctx: &ReqCtx,
    ok: bool,
    err: Option<&pingora::Error>,
    auth_user_id: &gw_core::ids::UserId,
    effective: &gw_core::ids::UserId,
    model: &gw_core::ids::ModelName,
    picked: &gw_core::entity::VendorKey,
) -> Option<UsageRecord> {
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
    let (usage, usage_provenance) = usage_snapshot_and_provenance_for_record(ctx, ok)?;

    // P0-32 — candidate chain: prior failed attempts (accumulated across
    // failover in `on_retry_sync`) plus this final attempt's key. The final
    // key carries no error_code on success; on failure it reuses the pingora
    // error etype, falling back to the per-attempt ctx signal.
    let final_attempt_error_code = if ok {
        None
    } else {
        err.map(|e| format!("{:?}", e.etype))
            .or_else(|| crate::billing_lifecycle::ctx_error_code(ctx))
    };
    let mut attempted_keys = ctx.attempted_keys.clone();
    attempted_keys.push(crate::billing_lifecycle::attempted_key_from(
        picked,
        model,
        ctx.attempt_index,
        final_attempt_error_code,
    ));

    Some(UsageRecord {
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
            .client_shape
            .map(|s| s.format_id().to_owned())
            .or_else(|| {
                ctx.client_codec
                    .as_ref()
                    .map(|c| c.kind().as_str().to_owned())
            })
            .unwrap_or_default(),
        upstream_protocol: ctx
            .upstream_shape
            .map(|s| s.format_id().to_owned())
            .or_else(|| {
                ctx.upstream_codec
                    .as_ref()
                    .map(|c| c.kind().as_str().to_owned())
            })
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
        usage,
        attempt_index: ctx.attempt_index,
        vendor_exposure_state: match ctx.vendor_exposure {
            crate::billing_lifecycle::VendorExposure::Settled => VendorExposureState::Settled,
            crate::billing_lifecycle::VendorExposure::Terminated => VendorExposureState::Terminated,
            _ => VendorExposureState::Terminated,
        },
        usage_provenance,
        latency_ms,
        ttft_ms,
        is_stream: ctx.is_stream,
        freeze_amount_micro: ctx
            .freeze_guard
            .as_ref()
            .map(|g| g.freeze_amount())
            .unwrap_or(0),
        estimate_pricing_version: ctx.estimate_pricing_version,
        markup_chain_snapshot: ctx
            .merged_perm
            .as_ref()
            .map(|m| m.markup_chain)
            .unwrap_or_else(MarkupChainSnapshot::identity),
        operation_id: ctx.operation_id.clone(),
        pod_id: gw_ledger::instance_id::cached().unwrap_or_default(),
        credits_consumed: ctx.credits_consumed,
        coef_table_version: ctx.coef_table_version_at_settle,
        attempted_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing_lifecycle::{transition_to, VendorExposure};
    use crate::ctx::ReqCtx;
    use gw_core::entity::VendorKey;
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::permissions::{MarkupChainSnapshot, MergedPermissions};
    use gw_core::protocol::ApiShape;
    use gw_ledger::{apply_coef_commit, apply_coef_entry, Dim};
    use secrecy::SecretString;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn seeded_vendor_key() -> VendorKey {
        VendorKey {
            key_id: KeyId::from("vk-1"),
            vendor_id: VendorId::from("vendor-1"),
            api_key: SecretString::from("secret".to_string()),
            status: gw_core::entity::KeyStatus::Active,
            accessible_models: vec![ModelName::from("gpt-4o")],
            model_mapping: std::collections::HashMap::new(),
            model_cost_weights: Default::default(),
            accessible_endpoints: vec![],
            protocol_conversion: false,
            priority: 0,
            weight: 1,
            rpm_limit: 0,
            tpm_limit: 0,
            max_usage_count: None,
            proxy: None,
            ip_allowlist: None,
            expires_at: None,
            tags: vec![],
            forced_capabilities: vec![],
            exclusive_capability_match: false,
            tunnel_preference: None,
            oauth: None,
            identity_snapshot: None,
            quota_state: Default::default(),
        }
    }

    fn seeded_cli_ctx() -> (ReqCtx, UserId, UserId, ModelName, VendorKey) {
        let mut ctx = ReqCtx {
            client_shape: Some(ApiShape::OpenAiCli),
            model: Some(ModelName::from("gpt-4o")),
            upstream_status: Some(200),
            merged_perm: Some(MergedPermissions {
                user_id: UserId::from("user-1"),
                effective_user_id: UserId::from("user-1"),
                client_ip: "127.0.0.1".parse().unwrap(),
                allowed_models: Default::default(),
                allowed_channels: Default::default(),
                allowed_groups: Default::default(),
                forced_model_group: None,
                rpm_limit: 0,
                tpm_limit: 0,
                max_concurrent_streams: 0,
                markup_chain: MarkupChainSnapshot::identity(),
                model_whitelist_regex: None,
            }),
            ..ReqCtx::default()
        };
        transition_to(&mut ctx, VendorExposure::Sent);
        transition_to(&mut ctx, VendorExposure::Streaming);
        transition_to(&mut ctx, VendorExposure::Settled);
        (
            ctx,
            UserId::from("user-1"),
            UserId::from("user-1"),
            ModelName::from("gpt-4o"),
            seeded_vendor_key(),
        )
    }

    fn next_test_coef_version() -> u64 {
        static NEXT_VERSION: AtomicU64 = AtomicU64::new(1_000_000);
        NEXT_VERSION.fetch_add(1, Ordering::Relaxed)
    }

    fn install_test_input_coef(version: u64, model_id: &str, input: u64) {
        apply_coef_entry(version, model_id, Dim::Input, input);
        let outcome = apply_coef_commit(version, 1);
        assert!(
            matches!(outcome, gw_ledger::CommitOutcome::Activated),
            "unexpected coef commit outcome: {outcome:?}"
        );
    }

    #[test]
    fn cli_final_logging_uses_nonzero_estimate_instead_of_free_usage() {
        let (mut ctx, auth_user_id, effective, model, picked) = seeded_cli_ctx();
        ctx.prompt_estimate_tokens = Some(41);
        ctx.usage_provenance.input = ProvenanceTag::CliExtracted;

        let rec =
            build_final_usage_record(&ctx, true, None, &auth_user_id, &effective, &model, &picked)
                .expect("final usage record");

        assert!(matches!(rec.status, UsageStatus::Ok));
        assert_eq!(rec.usage.input_tokens, 41);
        assert_eq!(rec.usage_provenance.input, ProvenanceTag::Estimated);
        assert!(rec.usage.input_tokens > 0 || rec.usage.output_tokens > 0);
    }

    #[test]
    fn cli_final_logging_without_estimate_skips_successful_free_record() {
        let (mut ctx, auth_user_id, effective, model, picked) = seeded_cli_ctx();
        ctx.prompt_estimate_tokens = None;

        let rec =
            build_final_usage_record(&ctx, true, None, &auth_user_id, &effective, &model, &picked);

        assert!(rec.is_none(), "final logging path must fail closed");
    }

    #[test]
    fn cli_final_logging_zero_cache_dimensions_use_nonzero_estimate() {
        let (mut ctx, auth_user_id, effective, model, picked) = seeded_cli_ctx();
        ctx.prompt_estimate_tokens = Some(19);
        ctx.usage.cached_tokens = Some(0);
        ctx.usage.cache_creation_tokens = Some(0);
        ctx.usage_provenance.cache_read = ProvenanceTag::CliExtracted;
        ctx.usage_provenance.cache_creation = ProvenanceTag::CliExtracted;

        let rec =
            build_final_usage_record(&ctx, true, None, &auth_user_id, &effective, &model, &picked)
                .expect("final usage record");

        assert_eq!(rec.usage.input_tokens, 19);
        assert_eq!(rec.usage_provenance.input, ProvenanceTag::Estimated);
        assert_eq!(rec.usage_provenance.cache_read, ProvenanceTag::Estimated);
        assert_eq!(
            rec.usage_provenance.cache_creation,
            ProvenanceTag::Estimated
        );
        assert!(rec.usage.input_tokens > 0 || rec.usage.output_tokens > 0);
    }

    #[test]
    fn cli_final_logging_zero_cache_dimensions_without_estimate_fails_closed() {
        let (mut ctx, auth_user_id, effective, model, picked) = seeded_cli_ctx();
        ctx.prompt_estimate_tokens = None;
        ctx.usage.cached_tokens = Some(0);
        ctx.usage.cache_creation_tokens = Some(0);
        ctx.usage_provenance.cache_read = ProvenanceTag::CliExtracted;
        ctx.usage_provenance.cache_creation = ProvenanceTag::CliExtracted;

        let rec =
            build_final_usage_record(&ctx, true, None, &auth_user_id, &effective, &model, &picked);

        assert!(
            rec.is_none(),
            "zero cache dims must not count as billable usage"
        );
    }

    #[test]
    fn protected_lease_settlement_uses_nonzero_billing_protected_usage_credits() {
        let (mut ctx, _, _, model, _) = seeded_cli_ctx();
        ctx.prompt_estimate_tokens = Some(23);
        ctx.usage.cached_tokens = Some(0);
        ctx.usage.cache_creation_tokens = Some(0);
        ctx.usage_provenance.cache_read = ProvenanceTag::CliExtracted;
        ctx.usage_provenance.cache_creation = ProvenanceTag::CliExtracted;

        let model_id = model.as_ref().to_string();
        let version = next_test_coef_version();
        install_test_input_coef(version, model_id.as_str(), 7);

        let (usage, provenance) =
            usage_snapshot_and_provenance_for_record(&ctx, true).expect("protected usage");
        assert_eq!(usage.input_tokens, 23);
        assert_eq!(provenance.input, ProvenanceTag::Estimated);
        assert_eq!(provenance.cache_read, ProvenanceTag::Estimated);
        assert_eq!(provenance.cache_creation, ProvenanceTag::Estimated);

        let actual = gw_ledger::actual_usage_credits(version, model_id.as_str(), &usage)
            .expect("actual usage credits");
        assert!(
            actual > 0,
            "protected settlement credits must stay billable"
        );
    }

    #[test]
    fn final_logging_records_single_attempted_key_on_success() {
        let (mut ctx, auth_user_id, effective, model, picked) = seeded_cli_ctx();
        ctx.usage.input_tokens = 10;
        ctx.usage_provenance.input = ProvenanceTag::UpstreamFinal;

        let rec =
            build_final_usage_record(&ctx, true, None, &auth_user_id, &effective, &model, &picked)
                .expect("final usage record");

        assert_eq!(rec.attempted_keys.len(), 1, "single-candidate success");
        let k = &rec.attempted_keys[0];
        assert_eq!(k.vendor_key_id, "vk-1");
        assert_eq!(k.vendor, "vendor-1");
        assert_eq!(k.model, "gpt-4o");
        assert_eq!(k.attempt_index, 0);
        assert!(
            k.error_code.is_none(),
            "successful final key must have no error_code"
        );
    }

    #[test]
    fn final_logging_preserves_failed_attempt_chain() {
        let (mut ctx, auth_user_id, effective, model, picked) = seeded_cli_ctx();
        ctx.usage.input_tokens = 10;
        ctx.usage_provenance.input = ProvenanceTag::UpstreamFinal;

        // Simulate one prior failed attempt, as `on_retry_sync` would have
        // accumulated into the persistent chain before failover.
        ctx.attempted_keys.push(gw_ledger::usage::AttemptedKey {
            vendor_key_id: "vk-prior".into(),
            vendor: "vendor-1".into(),
            model: "gpt-4o".into(),
            error_code: Some("GW_UPSTREAM_CONNECT_FAILED".into()),
            attempt_index: 0,
        });
        ctx.attempt_index = 1;

        let rec =
            build_final_usage_record(&ctx, true, None, &auth_user_id, &effective, &model, &picked)
                .expect("final usage record");

        assert_eq!(
            rec.attempted_keys.len(),
            2,
            "prior failed attempt + final key"
        );
        // Prior failed attempt preserved verbatim, in order.
        assert_eq!(rec.attempted_keys[0].vendor_key_id, "vk-prior");
        assert_eq!(rec.attempted_keys[0].attempt_index, 0);
        assert_eq!(
            rec.attempted_keys[0].error_code.as_deref(),
            Some("GW_UPSTREAM_CONNECT_FAILED")
        );
        // Final successful key appended with no error_code.
        assert_eq!(rec.attempted_keys[1].vendor_key_id, "vk-1");
        assert_eq!(rec.attempted_keys[1].attempt_index, 1);
        assert!(rec.attempted_keys[1].error_code.is_none());
    }
}
