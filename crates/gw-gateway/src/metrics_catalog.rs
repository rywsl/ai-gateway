//! Static catalog of all gateway metrics. Registered via `describe_all` so the
//! Prometheus scrape endpoint surfaces help text + types even before traffic.
//!
//! Cardinality: each label combination materializes a series; keep label sets
//! small (status-class collapse, not per-status-code). New metrics MUST be
//! added here so reviewers can see the full surface in one place.

use metrics::Unit;

pub struct MetricSpec {
    pub name: &'static str,
    pub kind: MetricKind,
    pub help: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

pub static METRICS: &[MetricSpec] = &[
    MetricSpec {
        name: "gw_codec_convert_total",
        kind: MetricKind::Counter,
        help: "codec conversion outcomes; labels=direction,source,target,status",
    },
    MetricSpec {
        name: "gw_codec_convert_duration_seconds",
        kind: MetricKind::Histogram,
        help: "codec conversion latency seconds",
    },
    MetricSpec {
        name: "gw_codec_passthrough_bytes_total",
        kind: MetricKind::Counter,
        help: "raw bytes passed through codec; label=direction",
    },
    MetricSpec {
        name: "gw_codec_cross_render_total",
        kind: MetricKind::Counter,
        help: "cross-protocol stream events re-rendered via IR round-trip (G8); labels=from,to",
    },
    MetricSpec {
        name: "gw_codec_cross_render_nonstream_total",
        kind: MetricKind::Counter,
        help: "Cross-protocol non-stream response re-renders performed (per from/to wire form).",
    },
    MetricSpec {
        name: "gw_codec_cross_render_lossy_total",
        kind: MetricKind::Counter,
        help: "Vendor-specific fields dropped during cross-protocol re-render (per from/to/field). \
               Each increment also emits a tracing::error! event.",
    },
    MetricSpec {
        name: "gw_codec_parse_response_failure_total",
        kind: MetricKind::Counter,
        help: "codec.parse_response returned an error on non-stream upstream body; labels=codec,upstream_status",
    },
    MetricSpec {
        name: "gw_codec_passthrough_total",
        kind: MetricKind::Counter,
        help: "same-protocol stream events forwarded verbatim (zero-copy); label=kind",
    },
    MetricSpec {
        name: "gw_stream_probe_total",
        kind: MetricKind::Counter,
        help: "stream probe decisions before client-visible commit; label=outcome",
    },
    MetricSpec {
        name: "gw_stream_probe_failover_total",
        kind: MetricKind::Counter,
        help: "stream probe cross-key failover outcomes; labels=reason,strategy,outcome",
    },
    MetricSpec {
        name: "gw_stream_probe_buffer_bytes",
        kind: MetricKind::Histogram,
        help: "bytes buffered by the pre-commit stream probe; no labels",
    },
    MetricSpec {
        name: "gw_stream_probe_timeout_total",
        kind: MetricKind::Counter,
        help: "stream probe timeout failures before client-visible commit; no labels",
    },
    MetricSpec {
        name: "gw_freeze_acquire_total",
        kind: MetricKind::Counter,
        help: "freeze acquire outcomes; label=outcome",
    },
    MetricSpec {
        name: "gw_freeze_release_total",
        kind: MetricKind::Counter,
        help: "freeze release outcomes; label=outcome",
    },
    MetricSpec {
        name: "gw_freeze_estimate_micro",
        kind: MetricKind::Histogram,
        help: "freeze estimate in micro-cents; label=model",
    },
    MetricSpec {
        name: "gw_auth_failure_total",
        kind: MetricKind::Counter,
        help: "auth failures; label=reason",
    },
    MetricSpec {
        name: "gateway_license_violation_total",
        kind: MetricKind::Counter,
        help: "License violations gated by [flags].license_enforce; labels=mode (disabled|warn|enforce), reason (expired|feature_missing|node_count_exceeded)",
    },
    MetricSpec {
        name: "gw_target_filter_denied_total",
        kind: MetricKind::Counter,
        help: "outbound targets denied by filter; label=host",
    },
    MetricSpec {
        name: "gw_redos_timeout_total",
        kind: MetricKind::Counter,
        help: "regex match exceeded budget; no labels (aggregate — per-ApiKey disable is tracked in-memory, not in the metric)",
    },
    MetricSpec {
        name: "gw_adaptive_rpm_observe_total",
        kind: MetricKind::Counter,
        help: "adaptive RPM 429 observations recorded",
    },
    MetricSpec {
        name: "gw_adaptive_rpm_enforced_total",
        kind: MetricKind::Counter,
        help: "adaptive RPM admission denied a send (key over its learned ceiling)",
    },
    MetricSpec {
        name: "gw_routing_unknown_preset_total",
        kind: MetricKind::Counter,
        help: "model group referenced an unknown multi_dim routing preset; fell back to fixed_order; label=code (GW_ROUTING_UNKNOWN_PRESET)",
    },
    MetricSpec {
        name: "gw_codec_content_decode_failure_total",
        kind: MetricKind::Counter,
        help: "cross-protocol non-stream upstream body failed content-encoding decode; labels=encoding,reason (reason ∈ {io, brotli, too_large})",
    },
    MetricSpec {
        name: "gw_adaptive_rpm_rpm_detected_total",
        kind: MetricKind::Counter,
        help: "429 carried a vendor-specific RPM-class rate-limit header (Anthropic/OpenAI requests)",
    },
    MetricSpec {
        name: "gw_upstream_auth_header_invalid_total",
        kind: MetricKind::Counter,
        help: "vendor auth_template had a colon but an invalid header name; fell back to authorization",
    },
    MetricSpec {
        name: "gateway_cold_load_timeout_total",
        kind: MetricKind::Counter,
        help: "cold entity load exceeded its deadline; label=ns (pre-existing emit, registered 2026-05-30)",
    },
    MetricSpec {
        name: "gateway_pool_acquire_timeout_total",
        kind: MetricKind::Counter,
        help: "cold entity load hit a PG pool acquire timeout (transient contention, not a missing row); label=ns",
    },
    MetricSpec {
        name: "gw_error_passthrough_masked_total",
        kind: MetricKind::Counter,
        help: "upstream error message masked by an error-passthrough deny rule",
    },
    MetricSpec {
        name: "gw_adaptive_rpm_probe_total",
        kind: MetricKind::Counter,
        help: "adaptive RPM probe expansions after decay and quiet period",
    },
    MetricSpec {
        name: "gw_adaptive_rpm_released_total",
        kind: MetricKind::Counter,
        help: "adaptive RPM enforcement releases after confidence drops below threshold",
    },
    MetricSpec {
        name: "gw_adaptive_rpm_learned_value",
        kind: MetricKind::Gauge,
        help: "current learned adaptive RPM value for the most recently updated key context",
    },
    MetricSpec {
        name: "gw_adaptive_rpm_confidence",
        kind: MetricKind::Gauge,
        help: "current adaptive RPM confidence for the most recently updated key context",
    },
    MetricSpec {
        name: "gw_ip_blocklist_hit_total",
        kind: MetricKind::Counter,
        help: "IP blocklist deny matches",
    },
    MetricSpec {
        name: "gw_ip_allowlist_check_total",
        kind: MetricKind::Counter,
        help: "IP allowlist checks executed",
    },
    MetricSpec {
        name: "gw_ip_ratelimit_check_total",
        kind: MetricKind::Counter,
        help: "per-IP rate-limit checks executed",
    },
    MetricSpec {
        name: "gw_ip_ratelimit_entries_active",
        kind: MetricKind::Gauge,
        help: "active in-memory per-IP rate-limit entries",
    },
    MetricSpec {
        name: "gw_ip_ratelimit_sweep_total",
        kind: MetricKind::Counter,
        help: "per-IP rate-limit sweep passes executed",
    },
    MetricSpec {
        name: "gw_ip_blocklist_entries",
        kind: MetricKind::Gauge,
        help: "current IP blocklist snapshot size",
    },
    MetricSpec {
        name: "gw_ip_allowlist_entries",
        kind: MetricKind::Gauge,
        help: "current IP allowlist snapshot size",
    },
    MetricSpec {
        name: "gw_kms_state",
        kind: MetricKind::Gauge,
        help: "KMS state value; label=state (healthy/dual_read/reencrypting)",
    },
    MetricSpec {
        name: "gw_kms_reencrypt_progress",
        kind: MetricKind::Gauge,
        help: "KEK rotate progress 0..1",
    },
    MetricSpec {
        name: "gw_oauth_refresh_total",
        kind: MetricKind::Counter,
        help: "oauth refresh outcomes; labels=vendor,outcome",
    },
    MetricSpec {
        name: "gw_oauth_token_age_seconds",
        kind: MetricKind::Histogram,
        help: "oauth token age at use; label=vendor",
    },
    MetricSpec {
        name: "gw_capability_resolve_total",
        kind: MetricKind::Counter,
        help: "capability resolve outcomes; label=match",
    },
    MetricSpec {
        name: "gw_capability_unknown_total",
        kind: MetricKind::Counter,
        help: "capability unknown for vendor; label=vendor",
    },
    MetricSpec {
        name: "gw_resilience_redis_error_total",
        kind: MetricKind::Counter,
        help: "redis ops failed",
    },
    MetricSpec {
        name: "gw_resilience_freeze_fallback_total",
        kind: MetricKind::Counter,
        help: "freeze fallback used",
    },
    MetricSpec {
        name: "gw_resilience_cooldown_fallback_total",
        kind: MetricKind::Counter,
        help: "cooldown fallback used",
    },
    MetricSpec {
        name: "gw_detailed_log_channel_depth",
        kind: MetricKind::Gauge,
        help: "Detailed-archive mpsc channel depth",
    },
    MetricSpec {
        name: "gw_detailed_log_compress_seconds",
        kind: MetricKind::Histogram,
        help: "Compression duration per detailed-archive entry (labels: encoding)",
    },
    MetricSpec {
        name: "gw_detailed_log_drop_total",
        kind: MetricKind::Counter,
        help: "Detailed-archive entries dropped (labels: reason, kind)",
    },
    MetricSpec {
        name: "gw_detailed_log_emit_total",
        kind: MetricKind::Counter,
        help: "Detailed-archive entries emitted (labels: kind, encoding)",
    },
    MetricSpec {
        name: "gw_archive_emit_grace_flush_total",
        kind: MetricKind::Counter,
        help: "Archive surfaces force-flushed in partial mode for grace-expired requests (W6-T01)",
    },
    MetricSpec {
        name: "gw_detailed_log_payload_bytes",
        kind: MetricKind::Histogram,
        help: "Compressed payload size per detailed-archive entry (labels: kind, encoding)",
    },
    MetricSpec {
        name: "gw_detailed_log_pre_compress_bytes",
        kind: MetricKind::Histogram,
        help: "Uncompressed JSON size per detailed-archive entry (labels: kind)",
    },
    MetricSpec {
        name: "gw_usage_dlq_enqueued_total",
        kind: MetricKind::Counter,
        help: "usage records routed to usage:records:dlq on primary XADD failure; label=outcome (success/error)",
    },
    MetricSpec {
        name: "gw_usage_dlq_emergency_file_total",
        kind: MetricKind::Counter,
        help: "Emergency JSONL fallback writes for usage-record DLQ failures; labels=outcome(success|error)",
    },
    MetricSpec {
        name: "gw_usage_dlq_retry_queue_total",
        kind: MetricKind::Counter,
        help: "In-memory retry queue outcomes after Redis DLQ write failure; labels=outcome(enqueued|full|closed)",
    },
    MetricSpec {
        name: "gw_usage_dlq_backlog_pushed_total",
        kind: MetricKind::Counter,
        help: "DLQ terminal fallback pushed envelope into BillingBacklog after retry queue/disk fallback could not accept it.",
    },
    MetricSpec {
        name: "gw_sticky_bind_total",
        kind: MetricKind::Counter,
        help: "sticky strategy bound a new (conversation_id -> key_id) entry",
    },
    MetricSpec {
        name: "gw_sticky_hit_total",
        kind: MetricKind::Counter,
        help: "sticky strategy reused an existing binding",
    },
    MetricSpec {
        name: "gw_sticky_evict_total",
        kind: MetricKind::Counter,
        help: "sticky bindings evicted by moka TTL or LRU overflow",
    },
    MetricSpec {
        name: "gw_sticky_bindings_active",
        kind: MetricKind::Gauge,
        help: "approximate live sticky bindings in the moka cache",
    },
    MetricSpec {
        name: "gw_retry_attempt_total",
        kind: MetricKind::Counter,
        help: "fail_to_connect retry decisions (G10); label=outcome (recovered/exhausted/tls_blocked)",
    },
    MetricSpec {
        name: "gw_cli_cross_protocol_denied_total",
        kind: MetricKind::Counter,
        help: "CLI requests denied because no same-protocol upstream channel remained",
    },
    // --- Egress impersonation (Phase 4 / Task 5.2) ---
    MetricSpec {
        name: "gw_egress_impersonate_dispatch_total",
        kind: MetricKind::Counter,
        help: "upstream requests routed through the egress-impersonate loopback (Impersonate vendors)",
    },
    MetricSpec {
        name: "gw_egress_disabled_total",
        kind: MetricKind::Counter,
        help: "Impersonate vendor candidates dropped by the egress kill-switch (egress.disabled=true); increments per skipped candidate; label=profile (claude_code_chrome131/openai_codex/gemini_cli)",
    },
    MetricSpec {
        name: "gw_egress_requests_total",
        kind: MetricKind::Counter,
        help: "egress-impersonate dispatch results; labels=profile,result (success/failure)",
    },
    MetricSpec {
        name: "gw_egress_error_total",
        kind: MetricKind::Counter,
        help: "egress-impersonate transport errors; labels=profile,class (tls/connect_timeout/connect_refused/read_timeout/h2_protocol/body_truncated/proxy_error/panic/unknown)",
    },
    MetricSpec {
        name: "gw_egress_client_pool_total",
        kind: MetricKind::Counter,
        help: "wreq client pool events; label=event (hit/miss/evicted)",
    },
    MetricSpec {
        name: "gw_egress_handshake_seconds",
        kind: MetricKind::Histogram,
        help: "wreq Client build (BoringSSL emulation template compile) duration; label=profile",
    },
    MetricSpec {
        name: "gw_egress_loopback_rtt_seconds",
        kind: MetricKind::Histogram,
        help: "gateway-side egress dispatch->response-headers latency (includes upstream time in prod; Task 6.2 microbench is the pure-overhead SLO gate); no labels",
    },
    MetricSpec {
        name: "gw_egress_lazy_persist_total",
        kind: MetricKind::Counter,
        help: "identity-snapshot lazy-persist outcomes; label=result (ok/failed/deferred/dropped)",
    },
    MetricSpec {
        name: "gw_egress_identity_cache_total",
        kind: MetricKind::Counter,
        help: "impersonation identity cache events; labels=kind,event (turn_state hit/miss)",
    },
    MetricSpec {
        name: "gw_egress_supervisor_restart_total",
        kind: MetricKind::Counter,
        help: "egress loopback supervisor restarts; label=reason",
    },
    MetricSpec {
        name: "gw_tunnel_open_total",
        kind: MetricKind::Counter,
        help: "tunnel stream-open outcome per node; labels=node_id,result (ok/no_live/target_denied/err)",
    },
    MetricSpec {
        name: "gw_tunnel_resolve_total",
        kind: MetricKind::Counter,
        help: "tunnel decision outcome; label=result (direct/tunnel/denied/no_node/no_live)",
    },
    MetricSpec {
        name: "gw_tunnel_transport_present",
        kind: MetricKind::Gauge,
        help: "1 if ReconnectingTransport::current() is Some, else 0; label=node_id",
    },
    MetricSpec {
        name: "gw_tunnel_last_pong_age_seconds",
        kind: MetricKind::Gauge,
        help: "seconds since last pong from tunnel transport; label=node_id",
    },
    MetricSpec {
        name: "gw_tunnel_reconnect_total",
        kind: MetricKind::Counter,
        help: "reconnect supervisor attempts; labels=node_id,reason",
    },
    MetricSpec {
        name: "gw_tunnel_doh_resolve_total",
        kind: MetricKind::Counter,
        help: "SafeDnsResolver outcome; label=result (ok/blocked/nx)",
    },
    // --- OAuth (G3 / go-onboarding-gaps T0) ---
    MetricSpec {
        name: "gw_oauth_token_near_expiry_total",
        kind: MetricKind::Counter,
        help: "upstream requests dispatched with access_token within 60s of expires_at",
    },
    MetricSpec {
        name: "gw_oauth_token_expired_total",
        kind: MetricKind::Counter,
        help: "upstream requests dispatched with already-expired access_token",
    },
    MetricSpec {
        name: "gw_oauth_runtime_refresh_total",
        kind: MetricKind::Counter,
        help: "Rust OAuth runtime refresh outcomes; label=result (ok/lock_lost/cas_lost/provider_error)",
    },
    MetricSpec {
        name: "gw_oauth_runtime_writeback_total",
        kind: MetricKind::Counter,
        help: "Rust OAuth encrypted token write-back outcomes; label=result (ok/cas_lost/failed)",
    },
    MetricSpec {
        name: "gw_oauth_refresh_status_xadd_total",
        kind: MetricKind::Counter,
        help: "OAuth status-only XADDs to events:entity after Rust-owned refresh failures",
    },
    // --- Billing robustness W1 ---
    MetricSpec {
        name: "gw_billing_emit_total",
        kind: MetricKind::Counter,
        help: "UsageRecord emit count, labelled by vendor_exposure_state (W4-T1)",
    },
    MetricSpec {
        name: "gw_billing_provenance_total",
        kind: MetricKind::Counter,
        help: "Per-dimension provenance tag counts; locates which codec/dim has high Estimated rate",
    },
    // --- Billing robustness W2 ---
    MetricSpec {
        name: "gw_billing_instance_heartbeat_renewals_total",
        kind: MetricKind::Counter,
        help: "InstanceId heartbeat key renewal successes (Go-side reaper liveness signal)",
    },
    MetricSpec {
        name: "gw_billing_exposure_transitions_total",
        kind: MetricKind::Counter,
        help: "VendorExposure state machine transitions; labels=from,to",
    },
    MetricSpec {
        name: "gw_billing_inflight_register_failure_total",
        kind: MetricKind::Counter,
        help: "InflightLedger register() returned an error (Redis unreachable / write failed)",
    },
    MetricSpec {
        name: "gw_billing_emit_path_total",
        kind: MetricKind::Counter,
        help: "UsageRecord emit attempts at each fallback layer; a single record may increment multiple paths. Paths include channel, primary_full_attempt, primary_overflow_ok, overflow_full_dlq, overflow_closed_dlq, primary_closed_attempt, primary_closed_overflow_ok, primary_closed_overflow_full_dlq, primary_closed_overflow_closed_dlq.",
    },
    MetricSpec {
        name: "gw_billing_primary_overflow_pending",
        kind: MetricKind::Gauge,
        help: "Approximate UsageRecord count enqueued in primary overflow but not yet committed via XADD. Counts items that have left the overflow channel but are still in worker buf.",
    },
    MetricSpec {
        name: "gw_cli_usage_extract_total",
        kind: MetricKind::Counter,
        help: "CLI usage extraction successes; labels=shape,source(stream_event|response_body)",
    },
    MetricSpec {
        name: "gw_billing_inflight_clear_total",
        kind: MetricKind::Counter,
        help: "Outcome of the DEL inflight key step after XADD-confirmed ledger delivery; labels=outcome(ok|del_failed|skipped_dlq). skipped_lost retained only for legacy paths and should approach zero.",
    },
    MetricSpec {
        name: "gw_billing_flusher_panic_total",
        kind: MetricKind::Counter,
        help: "flush(...) panicked; the panicked batch is duplicate-routed to DLQ. ANY non-zero = page on-call.",
    },
    MetricSpec {
        name: "gw_billing_flusher_dead_total",
        kind: MetricKind::Counter,
        help: "Primary channel closed AND primary overflow closed simultaneously; must-deliver first layer fully down. Records still land in DLQ/backlog cascade. ANY non-zero = page on-call.",
    },
    MetricSpec {
        name: "gw_billing_overflow_buf_rescued_total",
        kind: MetricKind::Counter,
        help: "Overflow worker future was aborted with non-empty local buf or queued receiver items; rescued items routed to recoverable emergency DLQ file or BillingBacklog. Non-zero indicates abnormal worker termination. Best-effort: receiver-lock contention can skip a rescue, in which case items recover when the worker next acquires the receiver.",
    },
    // --- Billing robustness W3-T1 ---
    MetricSpec {
        name: "gw_billing_upstream_cancellation_total",
        kind: MetricKind::Counter,
        help: "Upstream cancellation events labelled by cause (client_close/deadline/upstream_err/route_abort)",
    },
    // --- Billing robustness W3-T5 ---
    MetricSpec {
        name: "gw_billing_grace_yielded_final_usage_total",
        kind: MetricKind::Counter,
        help: "Whether the cancel grace window captured a final usage frame; label=yielded(true/false)",
    },
    // --- Billing robustness W4-T3 ---
    MetricSpec {
        name: "gw_billing_emit_lag_t0_total",
        kind: MetricKind::Counter,
        help: "T0 incident counter — ts_settled_ms exceeded emit_lag_t0_secs without DEL. ANY non-zero value = page on-call.",
    },
    // --- Billing robustness W4-T4 ---
    MetricSpec {
        name: "gw_billing_inflight_recovered_total",
        kind: MetricKind::Counter,
        help: "Number of CrashFallback UsageRecords emitted by the startup reconciler",
    },
    // --- Gateway-wide operational counters ---
    MetricSpec {
        name: "gateway_started_total",
        kind: MetricKind::Counter,
        help: "Incremented once at process startup so /metrics is non-empty before traffic",
    },
    MetricSpec {
        name: "gateway_requests_total",
        kind: MetricKind::Counter,
        help: "Total proxy requests; label=status_class (2xx/4xx/5xx/err)",
    },
    MetricSpec {
        name: "gateway_tpm_consumed_total",
        kind: MetricKind::Counter,
        help: "Observed tokens fed into the per-user sliding-window TPM limiter; no labels (aggregate — per-user enforcement is owned by the limiter, not the metric)",
    },
    MetricSpec {
        name: "gateway_freeze_leak_recovered_total",
        kind: MetricKind::Counter,
        help: "Freeze guards recovered via Drop rollback (leak-safety net)",
    },
    // --- Usage flusher ---
    MetricSpec {
        name: "gateway_usage_records_lost_total",
        kind: MetricKind::Counter,
        help: "UsageRecords permanently lost (primary XADD + DLQ both failed)",
    },
    MetricSpec {
        name: "gateway_usage_channel_full_total",
        kind: MetricKind::Counter,
        help: "UsageRecord dropped because the mpsc channel was full",
    },
    MetricSpec {
        name: "gateway_usage_serialize_errors_total",
        kind: MetricKind::Counter,
        help: "UsageRecord serde_json serialization failures (record dropped)",
    },
    MetricSpec {
        name: "gateway_usage_queue_errors_total",
        kind: MetricKind::Counter,
        help: "Individual XADD queue failures in the pipelined usage flush batch",
    },
    MetricSpec {
        name: "gateway_usage_flush_errors_total",
        kind: MetricKind::Counter,
        help: "Pipelined usage flush RTT failures (entire batch routed to DLQ)",
    },
    MetricSpec {
        name: "gw_frozen_delta_enqueue_total",
        kind: MetricKind::Counter,
        help: "RequestFreeze frozen_micro delta enqueue outcomes; labels=outcome(ok|full|no_sink)",
    },
    MetricSpec {
        name: "gw_frozen_delta_redis_total",
        kind: MetricKind::Counter,
        help: "Redis counter:user:<uid>:frozen_micro sync outcomes; labels=outcome(ok|error)",
    },
    // --- Deduct GC ---
    MetricSpec {
        name: "gw_deduct_gc_removed_total",
        kind: MetricKind::Counter,
        help: "Deduction tracking entries garbage-collected (all dimensions zeroed)",
    },
    // --- Forced params ---
    MetricSpec {
        name: "gw_forced_params_applied_total",
        kind: MetricKind::Counter,
        help: "Forced parameter overrides applied to upstream request; label=field",
    },
    MetricSpec {
        name: "gw_model_rewrite_total",
        kind: MetricKind::Counter,
        help: "Model-name rewrites applied to upstream request bodies; labels=from,to",
    },
    MetricSpec {
        name: "gw_body_rules_applied_total",
        kind: MetricKind::Counter,
        help: "Per-endpoint body-rule defaults injected into upstream request; labels=endpoint,key",
    },
    // --- Error handler ---
    MetricSpec {
        name: "gw_classifier_outcome_total",
        kind: MetricKind::Counter,
        help: "Error classifier outcomes; label=class",
    },
    MetricSpec {
        name: "gw_error_handler_ledger_total",
        kind: MetricKind::Counter,
        help: "Error outcomes recorded by the ledger listener",
    },
    // --- Daily quota ---
    MetricSpec {
        name: "gw_daily_quota_increment_error_total",
        kind: MetricKind::Counter,
        help: "Redis INCRBY+EXPIRE failures when bumping per-user daily token counter",
    },
    // --- Task worker metrics (ADR-T9) ---
    MetricSpec {
        name: "gw_task_claimed_total",
        kind: MetricKind::Counter,
        help: "Tasks claimed from the queue by a worker",
    },
    MetricSpec {
        name: "gw_task_settled_total",
        kind: MetricKind::Counter,
        help: "Tasks settled by a worker; label=status (succeeded/failed/cancelled)",
    },
    MetricSpec {
        name: "gw_task_execution_duration_seconds",
        kind: MetricKind::Histogram,
        help: "Task execution wall-clock duration in seconds",
    },
    MetricSpec {
        name: "gw_task_lease_recovered_total",
        kind: MetricKind::Counter,
        help: "Expired task leases recovered by the watchdog",
    },
    MetricSpec {
        name: "gw_task_heartbeat_total",
        kind: MetricKind::Counter,
        help: "Task lease heartbeat renewals sent",
    },
    MetricSpec {
        name: "gw_task_cancel_received_total",
        kind: MetricKind::Counter,
        help: "Cancel events received and processed by the cancel loop",
    },
    // --- Task gateway metrics (ADR-T9) ---
    MetricSpec {
        name: "gw_task_gw_submit_total",
        kind: MetricKind::Counter,
        help: "Task submit endpoint calls (successful enqueue)",
    },
    MetricSpec {
        name: "gw_task_gw_freeze_total",
        kind: MetricKind::Counter,
        help: "Task freeze outcomes; label=result (ok/insufficient)",
    },
    MetricSpec {
        name: "gw_task_gw_load_total",
        kind: MetricKind::Counter,
        help: "Task load endpoint calls",
    },
    MetricSpec {
        name: "gw_task_gw_watch_total",
        kind: MetricKind::Counter,
        help: "Task watch endpoint calls",
    },
    MetricSpec {
        name: "gw_task_gw_cancel_total",
        kind: MetricKind::Counter,
        help: "Task cancel endpoint calls",
    },
    // --- Credit lease (2026-05-29 design) ---
    MetricSpec {
        name: "gw_credit_lease_acquire_total",
        kind: MetricKind::Counter,
        help: "Lease acquire attempts in protected mode, labeled by result (ok | insufficient | no_coefs)",
    },
    MetricSpec {
        name: "gw_credit_lease_expired_total",
        kind: MetricKind::Counter,
        help: "Requests rejected because lease TTL expired (Go push pipeline lag)",
    },
    MetricSpec {
        name: "gw_credit_lease_warmup_pending_total",
        kind: MetricKind::Counter,
        help: "Requests on a user not yet visible in LeasePool — fell through to legacy soft defense",
    },
    MetricSpec {
        name: "gw_credit_default_ceiling_used_total",
        kind: MetricKind::Counter,
        help: "Times the fallback output token ceiling was used during credit estimation",
    },
    MetricSpec {
        name: "gw_credit_actual_exceeds_reserved_total",
        kind: MetricKind::Counter,
        help: "Settle observed actual credits > reserved (estimator drift signal)",
    },
    MetricSpec {
        name: "gw_credit_coef_table_incomplete_total",
        kind: MetricKind::Counter,
        help: "coef_table commit arrived with fewer staged models than expected",
    },
    MetricSpec {
        name: "gw_credit_active_register_enqueue_total",
        kind: MetricKind::Counter,
        help: "Rust active-credit registration enqueue outcomes; label=result (queued|throttled|full|closed|disabled)",
    },
    MetricSpec {
        name: "gw_credit_active_register_redis_total",
        kind: MetricKind::Counter,
        help: "Rust active-credit registration Redis write outcomes; label=result (success|retry_exhausted)",
    },
    MetricSpec {
        name: "gw_credit_active_register_queue_depth",
        kind: MetricKind::Gauge,
        help: "Remaining bounded worker channel capacity for active-credit registration",
    },
    MetricSpec {
        name: "gw_credit_lease_sweep_removed_total",
        kind: MetricKind::Counter,
        help: "Removed expired local protected leases",
    },
    MetricSpec {
        name: "gw_credit_balance_fallback_total",
        kind: MetricKind::Counter,
        help: "OverflowGate balance fallback outcomes; labels=result (positive|zero|error), path (cache|pg_fallback)",
    },
    MetricSpec {
        name: "gw_credit_lease_active_users",
        kind: MetricKind::Gauge,
        help: "Number of users currently in protected mode (sampled)",
    },
    MetricSpec {
        name: "gw_credit_coef_table_version_active",
        kind: MetricKind::Gauge,
        help: "Currently active coef_table_version",
    },
    MetricSpec {
        name: "gw_error_message_rules_snapshot_version",
        kind: MetricKind::Gauge,
        help: "Current loaded error message rules snapshot version",
    },
    MetricSpec {
        name: "gw_error_message_rules_reload_total",
        kind: MetricKind::Counter,
        help: "Error message rules reload attempts; label=result(ok|error)",
    },
    MetricSpec {
        name: "gw_error_message_rule_match_total",
        kind: MetricKind::Counter,
        help: "Error message rule matches; label=rule_id",
    },
    MetricSpec {
        name: "gw_error_message_rule_no_match_total",
        kind: MetricKind::Counter,
        help: "Error message rule misses (no rule matched the request context)",
    },
    MetricSpec {
        name: "gw_error_message_rule_compile_error_total",
        kind: MetricKind::Counter,
        help: "Error message rule snapshot compile failures (snapshot rejected)",
    },
];

pub fn describe_all() {
    for m in METRICS {
        match m.kind {
            MetricKind::Counter => metrics::describe_counter!(m.name, Unit::Count, m.help),
            MetricKind::Gauge => metrics::describe_gauge!(m.name, m.help),
            MetricKind::Histogram => metrics::describe_histogram!(m.name, m.help),
        }
    }
}

/// Page-on-call billing counters that must be present in `/metrics` with a `0`
/// baseline *before* any incident. `describe_*` (see `describe_all`) only
/// registers HELP/TYPE — `metrics-exporter-prometheus` does not render a series
/// until a `counter!()` handle is created. Without the zero-init below these
/// counters are absent from `/metrics` until the bad event first fires, which
/// makes the Go-side `billing-rules.yml` alerts evaluate against an absent
/// series ("No data") instead of a healthy `0`. Each name here is documented as
/// "page on-call" in `METRICS`; `page_on_call_set_is_subset_of_catalog`
/// (tests/metrics_catalog.rs) keeps the two in sync.
pub static PAGE_ON_CALL_BILLING_COUNTERS: &[&str] = &[
    "gw_billing_flusher_panic_total",
    "gw_billing_flusher_dead_total",
    "gw_billing_emit_lag_t0_total",
];

/// Zero-initialize the page-on-call billing counters so their series render at
/// `0` immediately at boot. Call once, after `describe_all()`, during metrics
/// bootstrap. `increment(0)` is enough to materialize the (label-less) series;
/// it never perturbs a real incident count. For `gw_billing_emit_lag_t0_total`
/// the real emit carries `request_id`/`attempt_index` labels — this baseline is
/// the label-less family series that `sum(increase(...))` alerts key off of.
pub fn zero_init_page_on_call() {
    for name in PAGE_ON_CALL_BILLING_COUNTERS {
        metrics::counter!(*name).increment(0);
    }
}
