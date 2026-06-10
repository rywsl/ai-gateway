use gw_core::control_plane::{
    command_ack_hash_key, command_delivery_entity_key, default_gateway_capability,
    GatewayCommandAck, GatewayCommandAction, GatewayCommandDeliveryEntity, GatewayCommandStatus,
    GatewayCommandStreamEntry,
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
        .api_shapes
        .iter()
        .find(|shape| shape.api_shape == "kiro:eventstream")
        .expect("kiro:eventstream capability must be present");
    let kiro_cli = capability
        .api_shapes
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
