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
        name: "gw_target_filter_denied_total",
        kind: MetricKind::Counter,
        help: "outbound targets denied by filter; label=host",
    },
    MetricSpec {
        name: "gw_redos_timeout_total",
        kind: MetricKind::Counter,
        help: "regex match exceeded budget; label=api_key_id",
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
        name: "gw_detailed_log_oversized_total",
        kind: MetricKind::Counter,
        help: "detailed log dropped due to size",
    },
    MetricSpec {
        name: "gw_usage_dlq_enqueued_total",
        kind: MetricKind::Counter,
        help: "usage records routed to usage:records:dlq on primary XADD failure; label=outcome (success/error)",
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
        name: "gw_tunnel_transport_alive",
        kind: MetricKind::Gauge,
        help: "1 if ReconnectingTransport::current() is Some, else 0; label=node_id",
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
        name: "gw_oauth_refresh_failed_xadd_total",
        kind: MetricKind::Counter,
        help: "OAuthListener reverse XADDs to events:entity (op=status_change field=oauth_failed)",
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
