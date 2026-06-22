use std::sync::Arc;

use arc_swap::ArcSwap;
use fred::interfaces::HashesInterface;
use fred::types::{RedisKey, RedisMap, RedisValue};
use sqlx::{PgPool, Row};

use gw_core::error::message_rules::{CompiledErrorMessageRules, ErrorMessageRule};

use crate::redis_client::RedisCtx;

const NAMESPACE: &str = "error_message_rules";
const GLOBAL_ID: &str = "global";
const REDIS_KEY: &str = "entity:error_message_rules:global";
const PG_SQL: &str = r#"
SELECT id, enabled, priority, match_json, message, version
FROM error_message_rules
WHERE enabled = TRUE AND deleted_at IS NULL
ORDER BY priority ASC, id ASC
"#;

pub struct ErrorMessageRuleStore {
    redis: Option<RedisCtx>,
    pg: Option<PgPool>,
    current: ArcSwap<CompiledErrorMessageRules>,
}

impl ErrorMessageRuleStore {
    pub fn new(redis: RedisCtx, pg: PgPool) -> Self {
        Self {
            redis: Some(redis),
            pg: Some(pg),
            current: ArcSwap::new(empty_snapshot()),
        }
    }

    pub fn snapshot(&self) -> Arc<CompiledErrorMessageRules> {
        self.current.load_full()
    }

    pub async fn reload(&self) -> anyhow::Result<u64> {
        let loaded = self.load_rules().await;
        let (version, rules) = match loaded {
            Ok(loaded) => loaded,
            Err(err) => {
                metrics::counter!("gw_error_message_rules_reload_total", "result" => "error")
                    .increment(1);
                return Err(err);
            }
        };
        self.compile_and_store(version, rules)
    }

    fn compile_and_store(&self, version: u64, rules: Vec<ErrorMessageRule>) -> anyhow::Result<u64> {
        match CompiledErrorMessageRules::compile(rules) {
            Ok(compiled) => {
                self.current.store(Arc::new(compiled));
                let metric_version = u32::try_from(version).unwrap_or(u32::MAX);
                metrics::gauge!("gw_error_message_rules_snapshot_version").set(metric_version);
                metrics::counter!("gw_error_message_rules_reload_total", "result" => "ok")
                    .increment(1);
                Ok(version)
            }
            Err(err) => {
                tracing::warn!(
                    target: "gw_store::error_message_rules",
                    error = %err,
                    "error message rules snapshot rejected"
                );
                metrics::counter!("gw_error_message_rule_compile_error_total").increment(1);
                metrics::counter!("gw_error_message_rules_reload_total", "result" => "error")
                    .increment(1);
                Err(err.into())
            }
        }
    }

    async fn load_rules(&self) -> anyhow::Result<(u64, Vec<ErrorMessageRule>)> {
        if let Some(redis) = &self.redis {
            let client = redis.pool.next();
            let raw: RedisValue = client.hgetall(REDIS_KEY).await?;
            let hash = raw.into_map()?;
            if !hash.is_empty() {
                return decode_redis_snapshot(&hash);
            }
        }

        let pg = self
            .pg
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("error message rule store has no pg handle"))?;
        let rows = sqlx::query(PG_SQL).fetch_all(pg).await?;
        let mut rules = Vec::with_capacity(rows.len());
        let mut version = 0u64;
        for row in rows {
            let id: String = row.try_get("id")?;
            let enabled: bool = row.try_get("enabled")?;
            let priority: i32 = row.try_get("priority")?;
            let matcher: serde_json::Value = row.try_get("match_json")?;
            let message: String = row.try_get("message")?;
            let row_version: i64 = row.try_get("version")?;
            version = version.max(u64::try_from(row_version).unwrap_or(0));
            rules.push(ErrorMessageRule {
                id,
                enabled,
                priority: u32::try_from(priority).unwrap_or(0),
                matcher: serde_json::from_value(matcher)?,
                message,
            });
        }
        Ok((version, rules))
    }

    #[doc(hidden)]
    pub fn empty_with_rules(rules: Vec<ErrorMessageRule>) -> anyhow::Result<Self> {
        let compiled = CompiledErrorMessageRules::compile(rules)?;
        Ok(Self {
            redis: None,
            pg: None,
            current: ArcSwap::new(Arc::new(compiled)),
        })
    }

    #[doc(hidden)]
    pub fn empty_for_test() -> Self {
        Self {
            redis: None,
            pg: None,
            current: ArcSwap::new(empty_snapshot()),
        }
    }

    #[doc(hidden)]
    pub async fn replace_for_test(&self, rules: Vec<ErrorMessageRule>) -> anyhow::Result<u64> {
        self.compile_and_store(1, rules)
    }
}

fn empty_snapshot() -> Arc<CompiledErrorMessageRules> {
    Arc::new(
        CompiledErrorMessageRules::compile(Vec::new()).expect("empty rule set always compiles"),
    )
}

fn decode_redis_snapshot(hash: &RedisMap) -> anyhow::Result<(u64, Vec<ErrorMessageRule>)> {
    let v = redis_hash_get(hash, "v").ok_or_else(|| {
        anyhow::anyhow!("missing version field 'v' in entity:{NAMESPACE}:{GLOBAL_ID}")
    })?;
    let v = redis_value_to_u64(v).ok_or_else(|| {
        anyhow::anyhow!("invalid version field 'v' in entity:{NAMESPACE}:{GLOBAL_ID}")
    })?;

    let enc = redis_hash_get(hash, "enc").ok_or_else(|| {
        anyhow::anyhow!("missing encoding field 'enc' in entity:{NAMESPACE}:{GLOBAL_ID}")
    })?;
    let enc = redis_value_to_str(enc).ok_or_else(|| {
        anyhow::anyhow!("invalid encoding field 'enc' in entity:{NAMESPACE}:{GLOBAL_ID}")
    })?;
    anyhow::ensure!(
        enc == "json",
        "unsupported entity encoding '{enc}' in entity:{NAMESPACE}:{GLOBAL_ID}"
    );

    let d = redis_hash_get(hash, "d").ok_or_else(|| {
        anyhow::anyhow!("missing payload field 'd' in entity:{NAMESPACE}:{GLOBAL_ID}")
    })?;
    let payload = redis_value_to_bytes(d).ok_or_else(|| {
        anyhow::anyhow!("invalid payload field 'd' in entity:{NAMESPACE}:{GLOBAL_ID}")
    })?;
    let rules: Vec<ErrorMessageRule> = serde_json::from_slice(payload)?;
    Ok((v, rules))
}

fn redis_hash_get<'a>(hash: &'a RedisMap, field: &str) -> Option<&'a RedisValue> {
    match field {
        "v" => hash.get(&RedisKey::from_static_str("v")),
        "enc" => hash.get(&RedisKey::from_static_str("enc")),
        "d" => hash.get(&RedisKey::from_static_str("d")),
        _ => None,
    }
}

fn redis_value_to_u64(value: &RedisValue) -> Option<u64> {
    match value {
        RedisValue::Integer(v) => (*v).try_into().ok(),
        RedisValue::String(v) => v.parse().ok(),
        RedisValue::Bytes(v) => std::str::from_utf8(v).ok()?.parse().ok(),
        _ => None,
    }
}

fn redis_value_to_str(value: &RedisValue) -> Option<&str> {
    match value {
        RedisValue::String(v) => Some(std::ops::Deref::deref(v)),
        RedisValue::Bytes(v) => std::str::from_utf8(v).ok(),
        _ => None,
    }
}

fn redis_value_to_bytes(value: &RedisValue) -> Option<&[u8]> {
    match value {
        RedisValue::String(v) => Some(v.as_bytes()),
        RedisValue::Bytes(v) => Some(v.as_ref()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::error::message_rules::ErrorMessageMatcher;

    #[tokio::test]
    async fn bad_snapshot_keeps_old_compiled_rules() {
        let store = ErrorMessageRuleStore::empty_with_rules(vec![valid_rule("old")]).unwrap();
        assert_eq!(store.snapshot().rule_count(), 1);

        let err = store
            .replace_for_test(vec![invalid_rule_with_too_many_values()])
            .await
            .unwrap_err();

        assert!(err.to_string().contains("too many values"));
        assert_eq!(store.snapshot().rule_count(), 1);
    }

    #[test]
    fn decode_redis_envelope_accepts_go_shape() {
        let rules = vec![valid_rule("r1")];
        let map: RedisMap = vec![
            ("v".to_string(), "7".to_string()),
            ("enc".to_string(), "json".to_string()),
            ("d".to_string(), serde_json::to_string(&rules).unwrap()),
        ]
        .into_iter()
        .collect();

        let (version, decoded) = decode_redis_snapshot(&map).unwrap();
        assert_eq!(version, 7);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].id, "r1");
    }

    fn valid_rule(id: &str) -> ErrorMessageRule {
        ErrorMessageRule {
            id: id.to_string(),
            enabled: true,
            priority: 10,
            matcher: ErrorMessageMatcher {
                models: vec!["gpt-image-2-max".to_string()],
                upstream_message_contains: vec!["unsupported".to_string()],
                ..Default::default()
            },
            message: "use image endpoint".to_string(),
        }
    }

    fn invalid_rule_with_too_many_values() -> ErrorMessageRule {
        let mut rule = valid_rule("bad");
        rule.matcher.models = (0..51).map(|idx| format!("model-{idx}")).collect();
        rule
    }
}
