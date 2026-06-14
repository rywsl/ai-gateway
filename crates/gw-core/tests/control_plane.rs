use gw_core::control_plane::{
    command_ack_hash_key, command_delivery_entity_key, default_gateway_capability,
    gateway_capability, CommandConsumerState, GatewayCommandAck, GatewayCommandAction,
    GatewayCommandDeliveryEntity, GatewayCommandStatus, GatewayCommandStreamEntry,
    GatewayRuntimeSnapshot, RuntimeConfigSample, StreamLagSnapshot, CONTROL_PLANE_SCHEMA_VERSION,
};
use serde_json::json;

#[test]
fn command_stream_entry_is_index_only_delivery_notification() {
    let entry: GatewayCommandStreamEntry = serde_json::from_value(json!({
        "command_id": "cmd-1",
        "delivery_id": "cmd-1:gw-a",
        "target_instance_id": "gw-a",
        "delivery_mode": "durable",
        "issued_at_ms": 42
    }))
    .unwrap();

    assert_eq!(entry.command_id, "cmd-1");
    assert_eq!(entry.delivery_id, "cmd-1:gw-a");
    assert_eq!(entry.target_instance_id, "gw-a");
    assert_eq!(
        serde_json::to_value(entry.delivery_mode).unwrap(),
        json!("durable")
    );
}

#[test]
fn command_delivery_entity_decodes_go_owned_body() {
    let delivery: GatewayCommandDeliveryEntity = serde_json::from_value(json!({
        "command_id": "cmd-1",
        "delivery_id": "cmd-1:gw-a",
        "action": "cooldown-key",
        "target_scope": "all",
        "target_instance_id": "gw-a",
        "args": { "key_id": "vk-1", "format": "openai", "ttl_secs": 300 },
        "delivery_mode": "ephemeral",
        "issued_at_ms": 42,
        "expires_at_ms": 1042,
        "issued_by": "admin-1"
    }))
    .unwrap();

    assert_eq!(delivery.command_id, "cmd-1");
    assert_eq!(delivery.delivery_id, "cmd-1:gw-a");
    assert_eq!(delivery.action, GatewayCommandAction::CooldownKey);
    assert_eq!(delivery.target_scope.as_str(), "all");
    assert_eq!(delivery.target_instance_id, "gw-a");
    assert_eq!(delivery.args["format"], "openai");
    assert_eq!(delivery.expires_at_ms, Some(1042));
}

#[test]
fn command_ack_is_keyed_by_delivery_id_contract_fields() {
    let ack = GatewayCommandAck {
        command_id: "cmd-1".to_string(),
        delivery_id: "cmd-1:gw-a".to_string(),
        instance_id: "gw-a".to_string(),
        status: GatewayCommandStatus::Skipped,
        message: "command_entity_missing_or_cancelled".to_string(),
        finished_at_ms: 1234,
    };

    let value = serde_json::to_value(ack).unwrap();

    assert_eq!(value["command_id"], "cmd-1");
    assert_eq!(value["delivery_id"], "cmd-1:gw-a");
    assert_eq!(value["instance_id"], "gw-a");
    assert_eq!(value["status"], "skipped");
    assert_eq!(value["message"], "command_entity_missing_or_cancelled");
    assert_eq!(
        command_ack_hash_key("gw-a"),
        "gateway:instance:gw-a:cmd_ack"
    );
    assert_eq!(
        command_delivery_entity_key("cmd-1:gw-a"),
        "entity:gateway_command_delivery:cmd-1:gw-a"
    );
}

#[test]
fn capability_reports_kiro_shapes_and_disabled_commands_truthfully() {
    let capability = default_gateway_capability("gw-a", 100);

    let kiro_eventstream = capability
        .api_shape_capabilities
        .iter()
        .find(|shape| shape.api_shape == "kiro:eventstream")
        .expect("kiro:eventstream capability must be present");
    let kiro_cli = capability
        .api_shape_capabilities
        .iter()
        .find(|shape| shape.api_shape == "kiro:cli")
        .expect("kiro:cli capability must be present");
    let set_log_level = capability
        .commands
        .iter()
        .find(|cmd| cmd.action == GatewayCommandAction::SetLogLevel)
        .expect("set-log-level capability must be visible");
    let refresh_oauth = capability
        .commands
        .iter()
        .find(|cmd| cmd.action == GatewayCommandAction::RefreshOauth)
        .expect("refresh-oauth capability must be visible");

    assert!(kiro_eventstream.production_ready);
    assert!(!kiro_cli.production_ready);
    assert_eq!(kiro_cli.reason.as_deref(), Some("requires_real_cli_sample"));
    assert!(capability
        .api_shapes
        .iter()
        .any(|shape| shape == "kiro:cli"));
    assert!(capability
        .cli_variants
        .iter()
        .any(|shape| shape == "kiro:cli"));
    assert!(capability.kiro_cli.advertised);
    assert!(!capability.kiro_cli.production_enabled);
    assert!(!capability.kiro_cli.usage_ready);
    assert_eq!(
        capability.kiro_cli.reason.as_deref(),
        Some("requires_real_cli_sample")
    );
    assert!(!set_log_level.enabled);
    assert_eq!(
        set_log_level.reason.as_deref(),
        Some("runtime_log_reload_hook_missing")
    );
    assert!(!refresh_oauth.enabled);
    assert_eq!(
        refresh_oauth.reason.as_deref(),
        Some("oauth_force_refresh_hook_missing")
    );
}

#[test]
fn capability_serializes_legacy_shape_names_and_readiness_details() {
    let capability = default_gateway_capability("gw-a", 100);
    let value = serde_json::to_value(capability).unwrap();

    assert_eq!(value["instance_id"], "gw-a");
    assert_eq!(value["started_at_ms"], 100);
    assert_eq!(
        value["protocols"],
        json!(["openai", "anthropic", "gemini", "kiro"])
    );
    assert_eq!(value["api_shapes"][0], "openai:chat");
    assert!(value["api_shapes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|shape| shape.is_string()));
    assert_eq!(
        value["api_shape_capabilities"][5],
        json!({
            "api_shape": "kiro:cli",
            "readiness": "partial",
            "production_ready": false,
            "reason": "requires_real_cli_sample"
        })
    );
    assert_eq!(value["cli_variants"][4], "kiro:cli");
    assert_eq!(value["features"]["egress_impersonate"], true);
    assert_eq!(value["kiro_cli"]["advertised"], true);
    assert_eq!(value["kiro_cli"]["production_enabled"], false);
    assert_eq!(value["readiness"]["kiro_cli"]["usage_ready"], false);
}

#[test]
fn reset_cost_command_is_advertised_executable_with_wire_string() {
    assert_eq!(GatewayCommandAction::ResetCost.as_str(), "reset-cost");
    assert_eq!(
        serde_json::to_value(GatewayCommandAction::ResetCost).unwrap(),
        json!("reset-cost")
    );

    let capability = default_gateway_capability("gw-a", 100);
    let reset_cost = capability
        .commands
        .iter()
        .find(|cmd| cmd.action == GatewayCommandAction::ResetCost)
        .expect("reset-cost capability must be advertised");
    assert!(
        reset_cost.enabled,
        "reset-cost is executable so it must be advertised enabled"
    );
    assert!(reset_cost.reason.is_none());

    // A Go-issued delivery body decodes to the ResetCost action.
    let delivery: GatewayCommandDeliveryEntity = serde_json::from_value(json!({
        "command_id": "cmd-1",
        "delivery_id": "cmd-1:gw-a",
        "action": "reset-cost",
        "target_scope": "all",
        "target_instance_id": "gw-a",
        "args": { "key_id": "vk-1", "format": "openai" },
        "delivery_mode": "durable",
        "issued_at_ms": 42,
        "issued_by": "admin-1"
    }))
    .unwrap();
    assert_eq!(delivery.action, GatewayCommandAction::ResetCost);
}

#[test]
fn capability_preserves_started_at_when_refreshing_snapshot() {
    let capability = gateway_capability("gw-a", "gw-test-version", 200, 100);

    assert_eq!(capability.gateway_version, "gw-test-version");
    assert_eq!(capability.generated_at_ms, 200);
    assert_eq!(capability.started_at_ms, 100);
}

#[test]
fn runtime_snapshot_carries_full_control_plane_status_fields() {
    let snapshot = GatewayRuntimeSnapshot {
        schema_version: CONTROL_PLANE_SCHEMA_VERSION,
        instance_id: "gw-a".to_string(),
        generated_at_ms: 100,
        ts_ms: 100,
        uptime_ms: 50,
        active_requests: 2,
        inflight_tokens: 3,
        provider_health: Vec::new(),
        key_health: Vec::new(),
        command_consumer: CommandConsumerState {
            stream: "commands:gateway".to_string(),
            group: "group:gw-a".to_string(),
            consumer: "gw-a".to_string(),
            last_seen_ms: Some(90),
        },
        tunnel: Vec::new(),
        oauth_rotation: Vec::new(),
        impersonate_active: json!({}),
        inflight_total: 2,
        stream_lag: vec![StreamLagSnapshot {
            stream: "commands:gateway".to_string(),
            group: "group:gw-a".to_string(),
            pending: None,
            lag: None,
            last_seen_ms: Some(90),
        }],
        config_sample: RuntimeConfigSample {
            runtime_push_interval_ms: 2_000,
            snapshot_ttl_ms: 10_000,
            command_block_ms: 5_000,
        },
        readiness: Default::default(),
    };

    let value = serde_json::to_value(snapshot).unwrap();

    assert_eq!(value["ts_ms"], 100);
    assert_eq!(value["inflight_total"], 2);
    assert_eq!(value["tunnel"], json!([]));
    assert_eq!(value["oauth_rotation"], json!([]));
    assert_eq!(value["impersonate_active"], json!({}));
    assert_eq!(value["stream_lag"][0]["stream"], "commands:gateway");
    assert_eq!(value["config_sample"]["runtime_push_interval_ms"], 2_000);
    assert_eq!(value["readiness"]["runtime_snapshot"], false);
}
