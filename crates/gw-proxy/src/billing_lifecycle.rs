//! BillingTransaction state machine. Spec §4.1.
//!
//! The 5-state `VendorExposure` enum tracks how far the request has progressed
//! against the upstream vendor. `Cancelling` is intentionally absent — cancel
//! grace handling is internal to `UpstreamCancellationCoordinator` (added in
//! W3), the public state machine exposes only `Streaming → Terminated`.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VendorExposure {
    #[default]
    Idle,
    Sent,
    Streaming,
    Settled,
    Terminated,
}

pub fn can_transition(from: VendorExposure, to: VendorExposure) -> bool {
    use VendorExposure::*;
    matches!(
        (from, to),
        (Idle, Sent)
            | (Sent, Streaming)
            | (Sent, Terminated)
            | (Streaming, Settled)
            | (Streaming, Terminated)
    )
}

pub fn is_terminal(v: VendorExposure) -> bool {
    matches!(v, VendorExposure::Settled | VendorExposure::Terminated)
}

use crate::ctx::ReqCtx;
use gw_ledger::FreezeGuard as _;
use gw_ledger::{ClearKey, LedgerItem};

/// Apply a `VendorExposure` state transition to the ReqCtx.
///
/// Transitions are validated against [`can_transition`]; in debug builds an
/// invalid transition panics. In release, an invalid transition is logged at
/// warn level and the state change is still applied (defensive: don't crash
/// production on a bug we have a way to recover from). Same-state transitions
/// are tolerated (idempotent — no timestamp churn).
///
/// Records `Timing::sent_ms` on first reach of `Sent` and
/// `Timing::settled_ms` on first reach of `Settled` or `Terminated`.
/// Emits the `gw_billing_exposure_transitions_total{from,to}` counter.
pub fn transition_to(ctx: &mut ReqCtx, to: VendorExposure) {
    let from = ctx.vendor_exposure;
    if from == to {
        return;
    }
    if !can_transition(from, to) {
        debug_assert!(false, "invalid transition {from:?} -> {to:?}");
        tracing::warn!(
            ?from,
            ?to,
            "invalid billing-lifecycle transition; applying anyway"
        );
    }
    let now_ms = || time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
    match to {
        VendorExposure::Sent if ctx.timing.sent_ms.is_none() => {
            ctx.timing.sent_ms = Some(now_ms());
        }
        VendorExposure::Settled | VendorExposure::Terminated if ctx.timing.settled_ms.is_none() => {
            ctx.timing.settled_ms = Some(now_ms());
        }
        _ => {}
    }
    ctx.vendor_exposure = to;
    metrics::counter!(
        "gw_billing_exposure_transitions_total",
        "from" => format!("{from:?}").to_lowercase(),
        "to" => format!("{to:?}").to_lowercase(),
    )
    .increment(1);
}

/// Build a `UsageRecord` capturing the current attempt's partial state.
///
/// Used by `on_retry_sync` (and indirectly by `on_retry`) to emit a billing
/// record for the failed attempt before resetting ctx for the next one.
/// Mirrors the construction in `filters/logging.rs`; fields that can't be
/// derived from ctx alone (e.g. `latency_ms` wall-clock) are computed here
/// from the timing fields already captured on `ReqCtx`.
///
/// Returns `None` when no upstream was ever picked (i.e. `ctx.picked_vendor_key`
/// is `None`), which matches the same guard used in the `logging` filter — no
/// UsageRecord is emitted for a request that never left the gateway.
pub fn build_attempt_record(ctx: &ReqCtx) -> Option<gw_ledger::usage::UsageRecord> {
    use gw_core::permissions::MarkupChainSnapshot;
    use gw_core::usage::UsageSnapshot;
    use gw_ledger::usage::{UsageRecord, UsageStatus, VendorExposureState};
    use std::time::{SystemTime, UNIX_EPOCH};

    let auth_user_id = ctx
        .merged_perm
        .as_ref()
        .map(|m| m.user_id.clone())
        .or_else(|| ctx.user.as_ref().map(|u| u.value.user_id.clone()))?;
    let effective_user_id = ctx
        .merged_perm
        .as_ref()
        .map(|m| m.effective_user_id.clone())
        .unwrap_or_else(|| auth_user_id.clone());
    let model = ctx.model.clone()?;
    let picked = ctx.picked_vendor_key.as_ref()?;

    let is_subaccount = effective_user_id != auth_user_id;

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

    Some(UsageRecord {
        ts_ms: now_ms,
        request_id: ctx.request_id.0.clone(),
        trace_id: ctx.trace_id.0.clone(),
        user_id: effective_user_id,
        child_id: if is_subaccount {
            Some(auth_user_id)
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
        model,
        vendor_id: picked.vendor_id.clone(),
        vendor_key_id: picked.key_id.clone(),
        // Connect-time failure: the request was never accepted by the vendor.
        status: UsageStatus::Error,
        error_code: None,
        http_status: ctx.upstream_status.unwrap_or(0),
        usage: UsageSnapshot {
            input_tokens: ctx.usage.input_tokens,
            output_tokens: ctx.usage.output_tokens,
            cache_read_input_tokens: ctx.usage.cached_tokens,
            cache_creation_input_tokens: ctx.usage.cache_creation_tokens,
            thinking_tokens: ctx.usage.thinking_tokens,
            multimodal_input_tokens: ctx.usage.multimodal_input_tokens,
        },
        attempt_index: ctx.attempt_index,
        vendor_exposure_state: match ctx.vendor_exposure {
            VendorExposure::Settled => VendorExposureState::Settled,
            // All non-Settled states (Idle, Sent, Streaming, Terminated) map to
            // Terminated — either because the transition hasn't happened yet
            // (on_retry_sync forces it first) or as a defensive fallback.
            _ => VendorExposureState::Terminated,
        },
        usage_provenance: ctx.usage_provenance,
        latency_ms,
        ttft_ms,
        is_stream: ctx.is_stream,
        freeze_amount_micro: ctx
            .freeze_guard
            .as_ref()
            .map(|g| g.freeze_amount())
            .unwrap_or(0),
        estimate_pricing_version: 0,
        markup_chain_snapshot: ctx
            .merged_perm
            .as_ref()
            .map(|m| m.markup_chain)
            .unwrap_or_else(MarkupChainSnapshot::identity),
        operation_id: ctx.operation_id.clone(),
        pod_id: gw_ledger::instance_id::cached().unwrap_or_default(),
        credits_consumed: ctx.credits_consumed,
        coef_table_version: ctx.coef_table_version_at_settle,
    })
}

/// Synchronous half of retry settlement. Called from `fail_to_connect`
/// (which is not `async`) before `e.set_retry(true)`.
///
/// Steps:
/// 1. Force `Terminated` on the current attempt if not already terminal.
/// 2. Build a `UsageRecord` for the failed attempt (returns `None` on
///    the `Idle` short-circuit — no upstream byte exchange happened).
/// 3. Reset transient per-attempt ctx fields and bump `attempt_index`.
///
/// The caller is responsible for spawning the async work (inflight
/// `mark_settled` + `clear` and ledger `emit`) with the returned record.
///
/// Spec §5.6.
pub fn on_retry_sync(ctx: &mut ReqCtx) -> Option<gw_ledger::usage::UsageRecord> {
    // Short-circuit: if no upstream contact was made, nothing to settle.
    if matches!(ctx.vendor_exposure, VendorExposure::Idle) {
        ctx.attempt_index = ctx.attempt_index.saturating_add(1);
        return None;
    }

    // Force Terminated so the record reflects a clean terminal state.
    if !is_terminal(ctx.vendor_exposure) {
        transition_to(ctx, VendorExposure::Terminated);
    }

    // Snapshot the record BEFORE resetting ctx.
    let record = build_attempt_record(ctx);

    // Reset per-attempt transient state.
    ctx.attempt_index = ctx.attempt_index.saturating_add(1);
    ctx.vendor_exposure = VendorExposure::Idle;
    ctx.timing.sent_ms = None;
    ctx.timing.settled_ms = None;
    ctx.usage = crate::ctx::UsageAccumulator::default();
    ctx.usage_provenance = gw_ledger::usage::UsageProvenance::default();
    ctx.cancel_observed_at_ms = None;
    ctx.cancel_cause = None;
    ctx.cancel_grace_active = false;
    ctx.upstream_status = None;

    record
}

/// Async full retry-settlement sequence. Called from contexts that have
/// access to `InflightLedger` and `Ledger` (e.g. integration tests or a
/// future async retry path).
///
/// Performs:
/// 1. `on_retry_sync` (state mutation + record build)
/// 2. `InflightLedger::mark_settled(Terminated)` for the previous attempt
/// 3. `Ledger::emit` for the partial UsageRecord, carrying the inflight
///    `ClearKey` so the flusher DELs the key after XADD-confirmed delivery
///
/// Spec §5.6 of billing-robustness; clear semantics tightened by spec
/// `2026-05-29-billing-record-durability` §3.6.
pub async fn on_retry(
    ctx: &mut ReqCtx,
    inflight: &gw_ledger::inflight::InflightLedger,
    ledger: &gw_ledger::flusher::Ledger,
) {
    use gw_ledger::inflight::SettleState;

    // Capture the request_id and the attempt_index BEFORE the sync step
    // bumps it, so we can key the inflight operations against the old attempt.
    let request_id = ctx.request_id.0.clone();
    let old_attempt = ctx.attempt_index;

    let record = on_retry_sync(ctx);

    if let Some(rec) = record {
        // Mark the inflight hash for the failed attempt.
        if let Err(e) = inflight
            .mark_settled(&request_id, old_attempt, SettleState::Terminated)
            .await
        {
            tracing::warn!(
                error = ?e,
                request_id = %request_id,
                attempt = old_attempt,
                "on_retry: inflight mark_settled failed"
            );
        }

        // Emit must-deliver billing record for the failed attempt,
        // carrying the ClearKey so the flusher DELs the inflight hash
        // after XADD-confirmed delivery.
        ledger
            .emit(LedgerItem {
                record: rec,
                clear: Some(ClearKey {
                    request_id: request_id.clone(),
                    attempt_index: old_attempt,
                }),
            })
            .await;

        // inflight.clear is NOT called here — see
        // docs/superpowers/specs/2026-05-29-billing-record-durability-design.md §3.6.
    }
}

/// Response-side counterpart to the connect-path settlement inlined in
/// `LlmGateway::fail_to_connect`. Synchronously runs `on_retry_sync` (force
/// Terminated → build the failed attempt's `UsageRecord` → bump `attempt_index`),
/// then spawns the async inflight `mark_settled` + ledger `emit` for the OLD
/// attempt. Extracted so the response-side failover retry arms (5xx/429 and
/// egress) settle prior attempts identically to the connect path — without this,
/// a header-only retryable failover never emitted the prior attempt's record and
/// the next `inflight.register` clobbered the same `attempt_index` key
/// (audit 2026-05-30 §1.2 / spec §5.6 AttemptManifest).
///
/// Takes owned `Arc`s (callers pass `self.inflight.clone()` / `self.ledger.clone()`)
/// so the work can move into the spawn.
pub fn settle_prior_attempt_spawn(
    ctx: &mut ReqCtx,
    inflight: std::sync::Arc<gw_ledger::inflight::InflightLedger>,
    ledger: std::sync::Arc<gw_ledger::Ledger>,
) {
    use gw_ledger::inflight::SettleState;
    let request_id = ctx.request_id.0.clone();
    // `on_retry_sync` bumps attempt_index; the OLD attempt is therefore the
    // post-bump value minus 1 — identical to the proven connect-path pattern.
    if let Some(rec) = on_retry_sync(ctx) {
        let old_attempt = ctx.attempt_index.saturating_sub(1);
        tokio::spawn(async move {
            if let Err(e) = inflight
                .mark_settled(&request_id, old_attempt, SettleState::Terminated)
                .await
            {
                tracing::warn!(
                    error = ?e,
                    request_id = %request_id,
                    attempt = old_attempt,
                    "settle_prior_attempt_spawn: inflight mark_settled failed"
                );
            }
            ledger
                .emit(LedgerItem {
                    record: rec,
                    clear: Some(ClearKey {
                        request_id: request_id.clone(),
                        attempt_index: old_attempt,
                    }),
                })
                .await;
            // inflight.clear is NOT called here — the flusher DELs after
            // XADD-confirmed delivery; a missed spawn is recovered by the
            // startup reconciler (CrashFallback). Mirrors fail_to_connect.
        });
    }
}
