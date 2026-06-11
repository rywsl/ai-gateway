//! Locks the Rust -> Go control-plane JSON paths against silent drift.
//!
//! Go reads these Redis payloads directly from
//! `gateway:instance:<id>:{capability,runtime}`.

use gw_core::control_plane::{
    default_gateway_capability, GatewayReadiness, GatewayRuntimeSnapshot, RuntimeConfigSample,
    StreamLagSnapshot, CONTROL_PLANE_SCHEMA_VERSION,
};
use serde_json::json;

#[test]
fn capability_snapshot_wire_paths_are_locked() {
    let value = serde_json::to_value(default_gateway_capability("gw-a", 100)).unwrap();

    assert_eq!(
        value,
        json!({
            "schema_version": CONTROL_PLANE_SCHEMA_VERSION,
            "instance_id": "gw-a",
            "gateway_version": env!("CARGO_PKG_VERSION"),
            "generated_at_ms": 100,
            "started_at_ms": 100,
            "protocols": ["openai", "anthropic", "gemini", "kiro"],
            "api_shapes": [
                "openai:chat",
                "openai:responses",
                "anthropic:messages",
                "gemini:generate",
                "kiro:eventstream",
                "kiro:cli",
                "claude:cli",
                "openai:cli",
                "openai:compact",
                "gemini:cli"
            ],
            "api_shape_capabilities": [
                {"api_shape": "openai:chat", "readiness": "ready", "production_ready": true},
                {"api_shape": "openai:responses", "readiness": "ready", "production_ready": true},
                {"api_shape": "anthropic:messages", "readiness": "ready", "production_ready": true},
                {"api_shape": "gemini:generate", "readiness": "ready", "production_ready": true},
                {"api_shape": "kiro:eventstream", "readiness": "ready", "production_ready": true},
                {
                    "api_shape": "kiro:cli",
                    "readiness": "partial",
                    "production_ready": false,
                    "reason": "requires_real_cli_sample"
                },
                {"api_shape": "claude:cli", "readiness": "ready", "production_ready": true},
                {"api_shape": "openai:cli", "readiness": "ready", "production_ready": true},
                {"api_shape": "openai:compact", "readiness": "ready", "production_ready": true},
                {"api_shape": "gemini:cli", "readiness": "ready", "production_ready": true}
            ],
            "cli_variants": [
                "claude:cli",
                "openai:cli",
                "openai:compact",
                "gemini:cli",
                "kiro:cli"
            ],
            "routing_strategies": [
                "FixedOrder",
                "RoundRobin",
                "LoadBalance",
                "WeightedRr",
                "CostFirst",
                "LatencyFirst",
                "CacheAffinity",
                "Sticky"
            ],
            "features": {
                "credit_lease": true,
                "detailed_log": true,
                "egress_impersonate": true,
                "task_worker": false,
                "tunnel": false
            },
            "impersonate_identities": [
                "claude_code_chrome131",
                "openai_codex",
                "gemini_cli"
            ],
            "commands": [
                {"action": "reload-config", "enabled": true},
                {"action": "cooldown-key", "enabled": true},
                {"action": "clear-cooldown", "enabled": true},
                {"action": "clear-cache", "enabled": true},
                {
                    "action": "set-log-level",
                    "enabled": false,
                    "reason": "runtime_log_reload_hook_missing"
                },
                {
                    "action": "refresh-oauth",
                    "enabled": false,
                    "reason": "oauth_force_refresh_hook_missing"
                },
                {
                    "action": "drain",
                    "enabled": false,
                    "reason": "graceful_drain_hook_missing"
                },
                {
                    "action": "undrain",
                    "enabled": false,
                    "reason": "graceful_drain_hook_missing"
                },
                {
                    "action": "restart-instance",
                    "enabled": false,
                    "reason": "graceful_restart_hook_missing"
                },
                {
                    "action": "rotate-kek",
                    "enabled": false,
                    "reason": "kek_rotation_command_hook_missing"
                }
            ],
            "config_namespaces": [
                "config_routing",
                "config_ratelimit",
                "config_auth",
                "config_billing",
                "config_protection",
                "config_control_plane"
            ],
            "readiness": {
                "capability_snapshot": true,
                "runtime_snapshot": false,
                "command_consumer": false,
                "kiro_cli": {
                    "advertised": true,
                    "detector_ready": false,
                    "production_enabled": false,
                    "usage_ready": false,
                    "reason": "requires_real_cli_sample"
                }
            },
            "kiro_cli": {
                "advertised": true,
                "detector_ready": false,
                "production_enabled": false,
                "usage_ready": false,
                "reason": "requires_real_cli_sample"
            }
        })
    );
}

#[test]
fn runtime_snapshot_readiness_wire_paths_are_locked() {
    let value = serde_json::to_value(GatewayRuntimeSnapshot {
        schema_version: CONTROL_PLANE_SCHEMA_VERSION,
        instance_id: "gw-a".to_string(),
        generated_at_ms: 100,
        ts_ms: 100,
        uptime_ms: 50,
        active_requests: 2,
        inflight_tokens: 3,
        provider_health: Vec::new(),
        key_health: Vec::new(),
        command_consumer: Default::default(),
        tunnel: Vec::new(),
        oauth_rotation: Vec::new(),
        impersonate_active: json!({}),
        inflight_total: 2,
        stream_lag: vec![StreamLagSnapshot {
            stream: "commands:gateway".to_string(),
            group: "group:gw-a".to_string(),
            pending: Some(0),
            lag: Some(0),
            last_seen_ms: Some(90),
        }],
        config_sample: RuntimeConfigSample {
            runtime_push_interval_ms: 2_000,
            snapshot_ttl_ms: 10_000,
            command_block_ms: 5_000,
        },
        readiness: GatewayReadiness {
            runtime_snapshot: true,
            command_consumer: true,
            last_command_ack_ms: Some(95),
            kiro_cli: gw_core::control_plane::default_kiro_cli_readiness(),
            ..Default::default()
        },
    })
    .unwrap();

    assert_eq!(value["readiness"]["runtime_snapshot"], true);
    assert_eq!(value["readiness"]["command_consumer"], true);
    assert_eq!(value["readiness"]["last_command_ack_ms"], 95);
    assert_eq!(
        value["readiness"]["kiro_cli"],
        json!({
            "advertised": true,
            "detector_ready": false,
            "production_enabled": false,
            "usage_ready": false,
            "reason": "requires_real_cli_sample"
        })
    );
    assert_eq!(value["stream_lag"][0]["stream"], "commands:gateway");
    assert_eq!(value["config_sample"]["snapshot_ttl_ms"], 10_000);
}
