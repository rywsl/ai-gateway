use super::state_writer::{now_ms, DynamicRuntimeCfg};
use super::ControlPlaneObserved;
use crate::config::ControlPlaneCfg;
use fred::interfaces::{HashesInterface, KeysInterface, StreamsInterface};
use fred::types::{RedisValue, XReadResponse};
use gw_core::control_plane::{
    command_ack_hash_key, command_delivery_entity_key, CommandDeliveryMode, GatewayCommandAck,
    GatewayCommandAction, GatewayCommandDeliveryEntity, GatewayCommandStatus,
    GatewayCommandStreamEntry,
};
use gw_core::ids::*;
use gw_core::protocol::ProtocolKind;
use gw_health::{CooldownDirective, CooldownReason, KeyHealthStore};
use gw_store::{redis_client::RedisCtx, EntityStore};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

const COMMAND_STREAM: &str = "commands:gateway";
const COUNT_PER_POLL: u64 = 32;
const ACK_TTL_SECS: i64 = 86_400;

pub struct CommandConsumer {
    pub redis: RedisCtx,
    pub store: Arc<EntityStore>,
    pub health: Arc<KeyHealthStore>,
    pub instance_id: String,
    pub group: String,
    pub consumer: String,
    pub dynamic: Arc<DynamicRuntimeCfg>,
    pub observed: Arc<ControlPlaneObserved>,
}

impl CommandConsumer {
    pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        gw_store::managed_loop::Supervisor::spawn(self)
    }

    async fn ensure_groups(&self) -> anyhow::Result<()> {
        let client = self.redis.pool.next();
        let _: Result<(), _> = client
            .xgroup_create(COMMAND_STREAM, &self.group, "$", true)
            .await;
        Ok(())
    }

    async fn apply_entry(&self, stream_id: &str, fields: &HashMap<String, String>) -> XackDecision {
        let entry = match stream_entry_from_fields(fields) {
            Ok(entry) => entry,
            Err(detail) => {
                tracing::warn!(
                    target: "gw_gateway::control_plane",
                    detail = %detail,
                    fields = ?fields,
                    "commands:gateway entry malformed; xack without command ACK because delivery_id is unreliable"
                );
                return XackDecision::Xack;
            }
        };

        if entry.target_instance_id != self.instance_id {
            return XackDecision::Xack;
        }

        let entity_key = command_delivery_entity_key(&entry.delivery_id);
        let entity = match self.read_command_entity(&entity_key).await {
            Ok(CommandEntityRead::Missing) => {
                return xack_after_ack_result(
                    self.write_ack(
                        &entry,
                        GatewayCommandStatus::Skipped,
                        "command_entity_missing_or_cancelled",
                    )
                    .await,
                    &entry.delivery_id,
                );
            }
            Ok(CommandEntityRead::Malformed(detail)) => {
                let outcome = CommandOutcome::failed(detail);
                return self.complete_entry(&entry, &entity_key, outcome).await;
            }
            Ok(CommandEntityRead::Parsed(entity)) => entity,
            Err(detail) => {
                tracing::warn!(
                    target: "gw_gateway::control_plane",
                    delivery_id = %entry.delivery_id,
                    detail = %detail,
                    "command delivery entity read failed; leaving stream entry pending"
                );
                return XackDecision::DoNotXack;
            }
        };

        let outcome = match entity.validate_stream_entry(&entry) {
            Ok(()) => self.dispatch(&entity).await,
            Err(err) => CommandOutcome::failed(err.to_string()),
        };
        let decision = self.complete_entry(&entry, &entity_key, outcome).await;
        if decision == XackDecision::Xack {
            self.observed
                .last_command_stream_ms
                .store(stream_id_head_ms(stream_id), Ordering::Release);
        }
        decision
    }

    async fn complete_entry(
        &self,
        entry: &GatewayCommandStreamEntry,
        entity_key: &str,
        outcome: CommandOutcome,
    ) -> XackDecision {
        if let Err(err) = self
            .write_ack(entry, outcome.status, &outcome.message)
            .await
        {
            tracing::warn!(
                target: "gw_gateway::control_plane",
                delivery_id = %entry.delivery_id,
                error = %err,
                "command ACK write failed; leaving stream entry pending"
            );
            return XackDecision::DoNotXack;
        }
        if let Err(err) = self.delete_entity(entity_key).await {
            tracing::warn!(
                target: "gw_gateway::control_plane",
                delivery_id = %entry.delivery_id,
                entity_key = %entity_key,
                error = %err,
                "command delivery entity delete failed after ACK; leaving stream entry pending"
            );
            return XackDecision::DoNotXack;
        }
        XackDecision::Xack
    }

    async fn read_command_entity(&self, key: &str) -> Result<CommandEntityRead, String> {
        let client = self.redis.pool.next();
        let hash: HashMap<String, String> = client
            .hgetall(key)
            .await
            .map_err(|err| format!("command_entity_read_failed:{err}"))?;
        Ok(classify_command_entity_hash(&hash))
    }

    async fn write_ack(
        &self,
        entry: &GatewayCommandStreamEntry,
        status: GatewayCommandStatus,
        message: &str,
    ) -> anyhow::Result<()> {
        let finished_at_ms = now_ms();
        let ack = GatewayCommandAck {
            command_id: entry.command_id.clone(),
            delivery_id: entry.delivery_id.clone(),
            instance_id: self.instance_id.clone(),
            status,
            message: message.to_string(),
            finished_at_ms,
        };
        let payload = serde_json::to_string(&ack)?;
        let client = self.redis.pool.next();
        let key = command_ack_hash_key(&self.instance_id);
        let _: () = client.hset(&key, (&entry.delivery_id, payload)).await?;
        let _: () = client.expire(&key, ACK_TTL_SECS).await?;
        self.observed
            .last_command_ack_ms
            .store(finished_at_ms as u64, Ordering::Release);
        Ok(())
    }

    async fn delete_entity(&self, key: &str) -> anyhow::Result<()> {
        let client = self.redis.pool.next();
        let deleted: i64 = client.del(key).await?;
        command_entity_delete_result(deleted).map_err(anyhow::Error::msg)
    }

    async fn dispatch(&self, entity: &GatewayCommandDeliveryEntity) -> CommandOutcome {
        if let Some(expires_at_ms) = entity.expires_at_ms {
            if matches!(entity.delivery_mode, CommandDeliveryMode::Ephemeral)
                && expires_at_ms <= now_ms()
            {
                return CommandOutcome::skipped("expired");
            }
        } else if matches!(entity.delivery_mode, CommandDeliveryMode::Ephemeral) {
            return CommandOutcome::failed("ephemeral_missing_expires_at_ms");
        }

        match entity.action {
            GatewayCommandAction::ReloadConfig => self.reload_config(&entity.args).await,
            GatewayCommandAction::CooldownKey => self.cooldown_key(&entity.args),
            GatewayCommandAction::ClearCooldown => self.clear_cooldown(&entity.args),
            GatewayCommandAction::ResetCost => self.reset_cost(&entity.args),
            GatewayCommandAction::ClearCache => self.clear_cache(&entity.args).await,
            GatewayCommandAction::SetLogLevel
            | GatewayCommandAction::RefreshOauth
            | GatewayCommandAction::Drain
            | GatewayCommandAction::Undrain
            | GatewayCommandAction::RestartInstance
            | GatewayCommandAction::RotateKek => {
                CommandOutcome::failed(format!("command_disabled:{}", entity.action.as_str()))
            }
        }
    }

    async fn reload_config(&self, args: &serde_json::Value) -> CommandOutcome {
        let args: ReloadConfigArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(err) => return CommandOutcome::failed(format!("invalid_args:{err}")),
        };
        if !matches!(
            args.table.as_str(),
            "*" | "control_plane" | "config_control_plane"
        ) {
            return CommandOutcome::failed("config_table_reload_hook_missing");
        }
        match self.reload_control_plane_config().await {
            Ok(()) => CommandOutcome::ok("control_plane_config_reloaded"),
            Err(detail) => CommandOutcome::failed(detail),
        }
    }

    async fn reload_control_plane_config(&self) -> Result<(), String> {
        let client = self.redis.pool.next();
        let mut last_error = "control_plane_config_entity_missing".to_string();
        for key in [
            "entity:config:config_control_plane",
            "entity:config:control_plane",
        ] {
            let hash: HashMap<String, String> = client
                .hgetall(key)
                .await
                .map_err(|err| format!("control_plane_config_read_failed:{err}"))?;
            if hash.is_empty() {
                continue;
            }
            let entity = match parse_config_entity_hash(&hash) {
                Ok(entity) => entity,
                Err(err) => {
                    last_error = err;
                    continue;
                }
            };
            return self
                .dynamic
                .update(
                    entity.runtime_push_interval_ms,
                    entity.snapshot_ttl_ms,
                    entity.command_block_ms,
                )
                .map_err(str::to_string);
        }
        Err(last_error)
    }

    fn cooldown_key(&self, args: &serde_json::Value) -> CommandOutcome {
        let args: CooldownArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(err) => return CommandOutcome::failed(format!("invalid_args:{err}")),
        };
        let format = match parse_command_format(args.format()) {
            Ok(format) => format,
            Err(detail) => return CommandOutcome::failed(detail),
        };
        self.health.apply_cooldown(
            &KeyId::from(args.key_id.as_str()),
            format,
            CooldownDirective {
                reason: CooldownReason::AdminForce,
                ttl_secs: args.ttl_secs,
                reset_consecutive: true,
            },
        );
        CommandOutcome::ok("cooldown_applied")
    }

    fn clear_cooldown(&self, args: &serde_json::Value) -> CommandOutcome {
        let args: ClearCooldownArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(err) => return CommandOutcome::failed(format!("invalid_args:{err}")),
        };
        let format = match parse_command_format(args.format()) {
            Ok(format) => format,
            Err(detail) => return CommandOutcome::failed(detail),
        };
        self.health
            .clear_cooldown(&KeyId::from(args.key_id.as_str()), format);
        CommandOutcome::ok("cooldown_cleared")
    }

    fn reset_cost(&self, args: &serde_json::Value) -> CommandOutcome {
        reset_cost_outcome(&self.health, args)
    }

    async fn clear_cache(&self, args: &serde_json::Value) -> CommandOutcome {
        let args: ClearCacheArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(err) => return CommandOutcome::failed(format!("invalid_args:{err}")),
        };
        match invalidate_cache(&self.store, args.ns(), &args.id).await {
            Ok(()) => CommandOutcome::ok("cache_cleared"),
            Err(detail) => CommandOutcome::failed(detail),
        }
    }
}

#[async_trait::async_trait]
impl gw_store::managed_loop::ManagedLoop for CommandConsumer {
    fn name(&self) -> &'static str {
        "gateway_command_consumer"
    }

    async fn ensure(&self) -> Result<(), anyhow::Error> {
        self.ensure_groups().await
    }

    async fn tick(&self) -> gw_store::managed_loop::Outcome {
        let client = self.redis.pool.next();
        let values = self.dynamic.snapshot();
        let raw: Result<RedisValue, fred::error::RedisError> = client
            .xreadgroup(
                &self.group,
                &self.consumer,
                Some(COUNT_PER_POLL),
                Some(values.command_block_ms),
                false,
                vec![COMMAND_STREAM],
                vec![">"],
            )
            .await;
        let value = match raw {
            Ok(value) => value,
            Err(err) if err.to_string().contains("NOGROUP") => {
                return gw_store::managed_loop::Outcome::Recover(
                    gw_store::managed_loop::RecoverReason::UpstreamResourceGone("NOGROUP"),
                );
            }
            Err(err) => {
                tracing::warn!(
                    target: "gw_gateway::control_plane",
                    error = %err,
                    "commands:gateway xreadgroup failed; backing off"
                );
                return gw_store::managed_loop::Outcome::Sleep(Duration::from_millis(500));
            }
        };
        if value.is_null() {
            return gw_store::managed_loop::Outcome::Continue;
        }
        let entries: XReadResponse<String, String, String, String> =
            match value.into_xread_response() {
                Ok(entries) => entries,
                Err(err) => {
                    tracing::warn!(
                        target: "gw_gateway::control_plane",
                        error = %err,
                        "commands:gateway reply parse failed; backing off"
                    );
                    return gw_store::managed_loop::Outcome::Sleep(Duration::from_millis(500));
                }
            };
        for (stream, stream_entries) in entries {
            for (entry_id, fields) in stream_entries {
                match self.apply_entry(&entry_id, &fields).await {
                    XackDecision::Xack => {
                        if let Err(err) = client
                            .xack::<u64, _, _, _>(&stream, &self.group, entry_id.clone())
                            .await
                        {
                            tracing::warn!(
                                target: "gw_gateway::control_plane",
                                error = %err,
                                entry_id = %entry_id,
                                "commands:gateway xack failed; entry may redeliver"
                            );
                        }
                    }
                    XackDecision::DoNotXack => {
                        tracing::warn!(
                            target: "gw_gateway::control_plane",
                            entry_id = %entry_id,
                            "commands:gateway entry left pending"
                        );
                    }
                }
            }
        }
        gw_store::managed_loop::Outcome::Continue
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum XackDecision {
    Xack,
    DoNotXack,
}

#[derive(Debug)]
enum CommandEntityRead {
    Missing,
    Malformed(String),
    Parsed(GatewayCommandDeliveryEntity),
}

fn classify_command_entity_hash(hash: &HashMap<String, String>) -> CommandEntityRead {
    match parse_command_entity_hash(hash) {
        Ok(entity) => CommandEntityRead::Parsed(entity),
        Err(detail) if detail == "command_entity_missing" => CommandEntityRead::Missing,
        Err(detail) => CommandEntityRead::Malformed(detail),
    }
}

fn parse_command_entity_hash(
    hash: &HashMap<String, String>,
) -> Result<GatewayCommandDeliveryEntity, String> {
    if hash.is_empty() {
        return Err("command_entity_missing".to_string());
    }
    let version = hash
        .get("v")
        .ok_or_else(|| "command_entity_missing_v".to_string())?;
    version
        .parse::<u64>()
        .map_err(|_| "command_entity_invalid_v".to_string())?;
    let enc = hash
        .get("enc")
        .ok_or_else(|| "command_entity_missing_enc".to_string())?;
    if enc != "json" {
        return Err("command_entity_unsupported_enc".to_string());
    }
    let data = hash
        .get("d")
        .ok_or_else(|| "command_entity_missing_d".to_string())?;
    serde_json::from_str(data).map_err(|err| format!("command_entity_invalid_d:{err}"))
}

fn stream_entry_from_fields(
    fields: &HashMap<String, String>,
) -> Result<GatewayCommandStreamEntry, String> {
    Ok(GatewayCommandStreamEntry {
        command_id: required_field(fields, "command_id")?.to_string(),
        delivery_id: required_field(fields, "delivery_id")?.to_string(),
        target_instance_id: required_field(fields, "target_instance_id")?.to_string(),
        delivery_mode: parse_delivery_mode(required_field(fields, "delivery_mode")?)?,
        issued_at_ms: required_field(fields, "issued_at_ms")?
            .parse()
            .map_err(|_| "stream_entry_invalid_issued_at_ms".to_string())?,
    })
}

fn required_field<'a>(fields: &'a HashMap<String, String>, field: &str) -> Result<&'a str, String> {
    fields
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("stream_entry_missing_{field}"))
}

fn parse_delivery_mode(value: &str) -> Result<CommandDeliveryMode, String> {
    match value {
        "durable" => Ok(CommandDeliveryMode::Durable),
        "ephemeral" => Ok(CommandDeliveryMode::Ephemeral),
        _ => Err("stream_entry_invalid_delivery_mode".to_string()),
    }
}

fn parse_config_entity_hash(
    hash: &HashMap<String, String>,
) -> Result<ControlPlaneConfigEntity, String> {
    if hash.get("enc").map(String::as_str) != Some("json") {
        return Err("control_plane_config_unsupported_enc".to_string());
    }
    let data = hash
        .get("d")
        .ok_or_else(|| "control_plane_config_missing_d".to_string())?;
    serde_json::from_str(data).map_err(|err| format!("control_plane_config_invalid_d:{err}"))
}

async fn invalidate_cache(store: &Arc<EntityStore>, ns: &str, id: &str) -> Result<(), String> {
    if let Some(redis) = store.loader_redis() {
        let redis_key = format!("entity:{ns}:{id}");
        let client = redis.pool.next();
        if let Err(err) = client.del::<i64, _>(&redis_key).await {
            tracing::warn!(
                target: "gw_gateway::control_plane",
                ns = %ns,
                id = %id,
                key = %redis_key,
                error = %err,
                "clear-cache Redis L1 delete failed; continuing with local cache invalidation"
            );
        }
    }

    match ns {
        "user" => store.user.invalidate(&UserId::from(id)).await,
        "user_group" => store.user_group.invalidate(&UserGroupId::from(id)).await,
        "apikey" => store.apikey.invalidate(&ApiKeyId::from(id)).await,
        "vendor" => store.vendor.invalidate(&VendorId::from(id)).await,
        "vendor_key" => store.invalidate_vendor_key(&KeyId::from(id)).await,
        "model_group" => store.model_group.invalidate(&Group::from(id)).await,
        "pricing" => store.pricing.invalidate(&ModelName::from(id)).await,
        "estimate_pricing" => {
            store
                .estimate_pricing
                .invalidate(&ModelName::from(id))
                .await
        }
        "model" => store.model.invalidate(&ModelName::from(id)).await,
        "auth_config" => {
            store.auth_config.invalidate(&id.to_string()).await;
        }
        "jwks" => store.jwks.invalidate(&Kid::from(id)).await,
        "ipblock" => store.ipblock.invalidate(&id.to_string()).await,
        "proxy_node" => store.proxy_node.invalidate(&id.to_string()).await,
        "management_token" => store.invalidate_mgmt_token(id).await,
        _ => return Err("unsupported_cache_namespace".to_string()),
    }
    Ok(())
}

fn parse_command_format(format: &str) -> Result<ProtocolKind, String> {
    match format {
        "openai" => Ok(ProtocolKind::OpenAi),
        "anthropic" => Ok(ProtocolKind::Anthropic),
        "gemini" => Ok(ProtocolKind::Gemini),
        "kiro" => Ok(ProtocolKind::Kiro),
        _ => Err("invalid_format".to_string()),
    }
}

fn reset_cost_outcome(health: &KeyHealthStore, args: &serde_json::Value) -> CommandOutcome {
    let args: ResetCostArgs = match serde_json::from_value(args.clone()) {
        Ok(args) => args,
        Err(err) => return CommandOutcome::failed(format!("invalid_args:{err}")),
    };
    let format = match parse_command_format(args.format()) {
        Ok(format) => format,
        Err(detail) => return CommandOutcome::failed(detail),
    };
    health.reset_runtime(&KeyId::from(args.key_id.as_str()), format);
    CommandOutcome::ok("cost_runtime_reset")
}

fn xack_after_ack_result<E: std::fmt::Display>(
    ack_result: Result<(), E>,
    delivery_id: &str,
) -> XackDecision {
    match ack_result {
        Ok(()) => XackDecision::Xack,
        Err(err) => {
            tracing::warn!(
                target: "gw_gateway::control_plane",
                delivery_id = %delivery_id,
                error = %err,
                "command ACK write failed; leaving stream entry pending"
            );
            XackDecision::DoNotXack
        }
    }
}

fn command_entity_delete_result(deleted: i64) -> Result<(), &'static str> {
    if deleted >= 0 {
        Ok(())
    } else {
        Err("command_entity_delete_returned_negative")
    }
}

fn stream_id_head_ms(stream_id: &str) -> u64 {
    stream_id
        .split_once('-')
        .map(|(head, _)| head)
        .unwrap_or(stream_id)
        .parse()
        .unwrap_or(0)
}

pub fn group_name(group_prefix: &str, instance_id: &str) -> String {
    format!("{group_prefix}:{instance_id}")
}

pub fn consumer_name(cfg: &ControlPlaneCfg, instance_id: &str) -> String {
    if cfg.command_consumer_name.trim().is_empty() {
        instance_id.to_string()
    } else {
        cfg.command_consumer_name.clone()
    }
}

#[derive(Deserialize)]
struct ControlPlaneConfigEntity {
    runtime_push_interval_ms: u64,
    snapshot_ttl_ms: u64,
    #[serde(default = "default_command_block_ms")]
    command_block_ms: u64,
}

fn default_command_block_ms() -> u64 {
    5_000
}

#[derive(Deserialize)]
struct ReloadConfigArgs {
    table: String,
}

#[derive(Deserialize)]
struct CooldownArgs {
    key_id: String,
    #[serde(default, alias = "api_format")]
    format: String,
    ttl_secs: u32,
}

impl CooldownArgs {
    fn format(&self) -> &str {
        self.format.as_str()
    }
}

#[derive(Deserialize)]
struct ClearCooldownArgs {
    key_id: String,
    #[serde(default, alias = "api_format")]
    format: String,
}

impl ClearCooldownArgs {
    fn format(&self) -> &str {
        self.format.as_str()
    }
}

#[derive(Deserialize)]
struct ResetCostArgs {
    key_id: String,
    #[serde(default, alias = "api_format")]
    format: String,
}

impl ResetCostArgs {
    fn format(&self) -> &str {
        self.format.as_str()
    }
}

#[derive(Deserialize)]
struct ClearCacheArgs {
    #[serde(default, alias = "namespace")]
    ns: String,
    id: String,
}

impl ClearCacheArgs {
    fn ns(&self) -> &str {
        self.ns.as_str()
    }
}

struct CommandOutcome {
    status: GatewayCommandStatus,
    message: String,
}

impl CommandOutcome {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            status: GatewayCommandStatus::Ok,
            message: message.into(),
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            status: GatewayCommandStatus::Failed,
            message: message.into(),
        }
    }

    fn skipped(message: impl Into<String>) -> Self {
        Self {
            status: GatewayCommandStatus::Skipped,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stream_entry_uses_delivery_contract_fields_only() {
        let fields = HashMap::from([
            ("command_id".to_string(), "cmd-1".to_string()),
            ("delivery_id".to_string(), "del-1".to_string()),
            ("target_instance_id".to_string(), "gw-a".to_string()),
            ("delivery_mode".to_string(), "durable".to_string()),
            ("issued_at_ms".to_string(), "42".to_string()),
        ]);

        let entry = stream_entry_from_fields(&fields).unwrap();

        assert_eq!(entry.delivery_id, "del-1");
        assert_eq!(entry.target_instance_id, "gw-a");
        assert_eq!(entry.delivery_mode, CommandDeliveryMode::Durable);
    }

    #[test]
    fn stream_entry_rejects_legacy_target_payload() {
        let fields = HashMap::from([
            ("command_id".to_string(), "cmd-1".to_string()),
            ("target".to_string(), "all".to_string()),
            ("delivery".to_string(), "durable".to_string()),
            ("issued_at_ms".to_string(), "42".to_string()),
        ]);

        let err = stream_entry_from_fields(&fields).unwrap_err();

        assert_eq!(err, "stream_entry_missing_delivery_id");
    }

    #[test]
    fn command_entity_key_uses_delivery_id() {
        assert_eq!(
            command_delivery_entity_key("del-1"),
            "entity:gateway_command_delivery:del-1"
        );
    }

    #[test]
    fn parse_command_entity_decodes_delivery_payload() {
        let payload = json!({
            "command_id": "cmd-1",
            "delivery_id": "del-1",
            "action": "clear-cache",
            "target_scope": "all",
            "target_instance_id": "gw-a",
            "args": { "ns": "model", "id": "gpt-test" },
            "delivery_mode": "durable",
            "issued_at_ms": 42,
            "issued_by": "admin"
        });
        let hash = HashMap::from([
            ("v".to_string(), "1".to_string()),
            ("enc".to_string(), "json".to_string()),
            ("d".to_string(), payload.to_string()),
        ]);

        let entity = parse_command_entity_hash(&hash).unwrap();

        assert_eq!(entity.delivery_id, "del-1");
        assert_eq!(entity.action, GatewayCommandAction::ClearCache);
    }

    #[test]
    fn malformed_entity_is_not_missing() {
        let hash = HashMap::from([
            ("v".to_string(), "1".to_string()),
            ("enc".to_string(), "zstd".to_string()),
            ("d".to_string(), "{}".to_string()),
        ]);

        assert!(matches!(
            classify_command_entity_hash(&hash),
            CommandEntityRead::Malformed(detail) if detail == "command_entity_unsupported_enc"
        ));
    }

    #[test]
    fn delivery_validation_rejects_metadata_mismatch() {
        let entry = GatewayCommandStreamEntry {
            command_id: "cmd-1".to_string(),
            delivery_id: "del-stream".to_string(),
            target_instance_id: "gw-a".to_string(),
            delivery_mode: CommandDeliveryMode::Durable,
            issued_at_ms: 42,
        };
        let entity = GatewayCommandDeliveryEntity {
            command_id: "cmd-1".to_string(),
            delivery_id: "del-entity".to_string(),
            action: GatewayCommandAction::ClearCache,
            target_scope: gw_core::control_plane::GatewayCommandTargetScope::All,
            target_instance_id: "gw-a".to_string(),
            args: json!({}),
            delivery_mode: CommandDeliveryMode::Durable,
            issued_at_ms: 42,
            expires_at_ms: None,
            issued_by: None,
        };

        let err = entity.validate_stream_entry(&entry).unwrap_err();

        assert_eq!(err.to_string(), "delivery_id_mismatch");
    }

    #[test]
    fn ack_failure_keeps_stream_pending() {
        let decision = xack_after_ack_result(Err("ack failed"), "del-1");

        assert_eq!(decision, XackDecision::DoNotXack);
    }

    #[test]
    fn delete_zero_is_idempotent_after_ack() {
        assert_eq!(command_entity_delete_result(1), Ok(()));
        assert_eq!(command_entity_delete_result(0), Ok(()));
        assert_eq!(
            command_entity_delete_result(-1),
            Err("command_entity_delete_returned_negative")
        );
    }

    #[test]
    fn group_and_consumer_names_are_instance_scoped() {
        let cfg = ControlPlaneCfg::default();

        assert_eq!(group_name(&cfg.command_group, "gw-a"), "group:gw-a");
        assert_eq!(consumer_name(&cfg, "gw-a"), "gw-a");
    }

    #[test]
    fn config_entity_uses_latest_field_names() {
        let hash = HashMap::from([
            ("v".to_string(), "1".to_string()),
            ("enc".to_string(), "json".to_string()),
            (
                "d".to_string(),
                json!({
                    "runtime_push_interval_ms": 2000,
                    "snapshot_ttl_ms": 10000,
                    "command_block_ms": 5000
                })
                .to_string(),
            ),
        ]);

        let entity = parse_config_entity_hash(&hash).unwrap();

        assert_eq!(entity.runtime_push_interval_ms, 2_000);
        assert_eq!(entity.snapshot_ttl_ms, 10_000);
        assert_eq!(entity.command_block_ms, 5_000);
    }

    #[test]
    fn cooldown_args_accept_api_format_alias() {
        let args: CooldownArgs = serde_json::from_value(json!({
            "key_id": "vk-1",
            "api_format": "openai",
            "ttl_secs": 60
        }))
        .unwrap();

        assert_eq!(args.format(), "openai");
    }

    #[test]
    fn reset_cost_outcome_resets_runtime_health_state() {
        let health = KeyHealthStore::default();
        let key = KeyId::from("vk-reset-command");
        for _ in 0..21 {
            health.record(
                &key,
                ProtocolKind::OpenAi,
                gw_health::OutcomeKind::Failure {
                    http_status: 500,
                    retry_after_secs: None,
                },
            );
        }
        assert!(!health.is_available(&key, ProtocolKind::OpenAi));

        let outcome = reset_cost_outcome(
            &health,
            &json!({
                "key_id": "vk-reset-command",
                "api_format": "openai"
            }),
        );

        assert_eq!(outcome.status, GatewayCommandStatus::Ok);
        assert_eq!(outcome.message, "cost_runtime_reset");
        assert!(health.is_available(&key, ProtocolKind::OpenAi));
    }

    #[test]
    fn reset_cost_outcome_rejects_bad_args() {
        let health = KeyHealthStore::default();
        // Missing the required `key_id` field → deserialization failure.
        let outcome = reset_cost_outcome(&health, &json!({ "api_format": "openai" }));

        assert_eq!(outcome.status, GatewayCommandStatus::Failed);
        assert!(
            outcome.message.starts_with("invalid_args:"),
            "expected invalid_args prefix, got {}",
            outcome.message
        );
    }

    #[test]
    fn reset_cost_args_accept_api_format_alias() {
        let args: ResetCostArgs = serde_json::from_value(json!({
            "key_id": "vk-1",
            "api_format": "anthropic"
        }))
        .unwrap();

        assert_eq!(args.format(), "anthropic");
    }

    #[test]
    fn reset_cost_outcome_rejects_unknown_format() {
        let health = KeyHealthStore::default();
        let outcome = reset_cost_outcome(
            &health,
            &json!({
                "key_id": "vk-1",
                "api_format": "bogus"
            }),
        );

        assert_eq!(outcome.status, GatewayCommandStatus::Failed);
        assert_eq!(outcome.message, "invalid_format");
    }
}
