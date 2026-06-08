use crate::entity_store::EntityStore;
use crate::redis_client::RedisCtx;
use fred::interfaces::{KeysInterface, StreamsInterface};
use gw_core::entity::{QuotaAvailability, QuotaState, VendorKey};
use secrecy::ExposeSecret;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct QuotaStateWriter {
    pg: PgPool,
    redis: RedisCtx,
}

impl QuotaStateWriter {
    pub fn new(pg: PgPool, redis: RedisCtx) -> Self {
        Self { pg, redis }
    }

    pub async fn write(&self, key_id: &str, state: &QuotaState) -> anyhow::Result<()> {
        sqlx::query(update_sql())
            .bind(key_id)
            .bind(availability_str(state.availability))
            .bind(state.remaining)
            .bind(state.limit)
            .bind(state.reset_at_unix_s)
            .bind(state.observed_at_unix_s)
            .execute(&self.pg)
            .await?;

        let client = self.redis.pool.next();
        let redis_key = format!("entity:vendor_key:{key_id}");
        let _: Result<(), _> = client.del(redis_key).await;
        let _: Result<String, _> = client
            .xadd(
                "events:entity",
                false,
                None::<()>,
                fred::types::XID::Auto,
                vec![
                    ("ns".to_string(), "vendor_key".to_string()),
                    ("id".to_string(), key_id.to_string()),
                    ("op".to_string(), "quota_state".to_string()),
                ],
            )
            .await;
        Ok(())
    }
}

pub fn update_sql() -> &'static str {
    "UPDATE vendor_keys \
     SET quota_availability = $2, \
         quota_remaining = $3, \
         quota_limit = $4, \
         quota_reset_at = CASE WHEN $5::BIGINT IS NULL THEN NULL ELSE to_timestamp($5::BIGINT) END, \
         quota_observed_at = CASE WHEN $6::BIGINT IS NULL THEN NULL ELSE to_timestamp($6::BIGINT) END, \
         payload = jsonb_set(payload, '{quota_state}', jsonb_build_object(\
             'availability', $2::TEXT, \
             'remaining', $3::BIGINT, \
             'limit', $4::BIGINT, \
             'reset_at_unix_s', $5::BIGINT, \
             'observed_at_unix_s', $6::BIGINT, \
             'source', 'quota_state_writer'\
         ), true), \
         version = version + 1, \
         updated_at = NOW() \
     WHERE id = $1 AND deleted_at IS NULL"
}

#[derive(Clone)]
pub struct QuotaStateUpdateQueue {
    writer: Arc<QuotaStateWriter>,
    max_pending: usize,
    pending: Arc<dashmap::DashSet<String>>,
}

impl QuotaStateUpdateQueue {
    pub fn new(writer: Arc<QuotaStateWriter>, max_pending: usize) -> Self {
        Self {
            writer,
            max_pending,
            pending: Arc::new(dashmap::DashSet::new()),
        }
    }

    pub fn schedule(&self, key_id: String, state: QuotaState) {
        if self.pending.len() >= self.max_pending {
            metrics::counter!("gw_quota_state_update_total", "result" => "dropped").increment(1);
            return;
        }
        if !self.pending.insert(key_id.clone()) {
            metrics::counter!("gw_quota_state_update_total", "result" => "deduped").increment(1);
            return;
        }

        let writer = Arc::clone(&self.writer);
        let pending = Arc::clone(&self.pending);
        tokio::spawn(async move {
            match writer.write(&key_id, &state).await {
                Ok(()) => {
                    metrics::counter!("gw_quota_state_update_total", "result" => "ok").increment(1)
                }
                Err(e) => {
                    tracing::warn!(
                        target: "gw_store::quota_state",
                        key_id = %key_id,
                        error = %e,
                        "quota state write failed"
                    );
                    metrics::counter!("gw_quota_state_update_total", "result" => "failed")
                        .increment(1);
                }
            }
            pending.remove(&key_id);
        });
    }
}

#[derive(Clone, Debug)]
pub struct QuotaStateProbeCfg {
    pub enabled: bool,
    pub interval_secs: u64,
    pub probe_endpoint: String,
    pub max_pending: usize,
}

pub struct QuotaStateProbeRunner {
    pub store: Arc<EntityStore>,
    pub queue: Arc<QuotaStateUpdateQueue>,
    pub client: reqwest::Client,
    pub cfg: QuotaStateProbeCfg,
}

impl QuotaStateProbeRunner {
    pub fn start(self) {
        if !self.cfg.enabled {
            return;
        }
        tokio::spawn(async move {
            let interval = Duration::from_secs(self.cfg.interval_secs.max(1));
            loop {
                self.run_once().await;
                tokio::time::sleep(interval).await;
            }
        });
    }

    pub async fn run_once(&self) {
        let now = unix_s();
        for entry in self.store.vendor_key.iter() {
            let key = entry.1;
            if key.value.quota_state.availability != QuotaAvailability::Exhausted {
                continue;
            }
            if !key.value.quota_state.is_routable_at_unix_s(now) {
                continue;
            }
            self.probe_key(&key.value).await;
        }
    }

    async fn probe_key(&self, key: &VendorKey) {
        let Some(url) = self.probe_url(key).await else {
            return;
        };
        let response = self
            .client
            .get(url)
            .bearer_auth(key.api_key.expose_secret())
            .send()
            .await;
        let Ok(response) = response else {
            return;
        };
        if !response.status().is_success() {
            return;
        }
        let headers = ReqwestHeaders(response.headers());
        if let Some(state) = quota_state_from_headers(&headers) {
            self.queue.schedule(key.key_id.0.clone(), state);
        }
    }

    async fn probe_url(&self, key: &VendorKey) -> Option<reqwest::Url> {
        let vendor = self.store.vendor_or_load(&key.vendor_id).await?;
        let base = vendor.value.base_urls.first()?.clone();
        base.join(self.cfg.probe_endpoint.trim_start_matches('/'))
            .ok()
    }
}

trait HeaderView {
    fn get(&self, name: &str) -> Option<&str>;
}

struct ReqwestHeaders<'a>(&'a reqwest::header::HeaderMap);

impl HeaderView for ReqwestHeaders<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name)?.to_str().ok()
    }
}

fn quota_state_from_headers(headers: &dyn HeaderView) -> Option<QuotaState> {
    let remaining = first_i64(
        headers,
        &[
            "anthropic-ratelimit-requests-remaining",
            "x-ratelimit-remaining-requests",
            "x-rate-limit-remaining",
            "ratelimit-remaining",
        ],
    )?;
    let limit = first_i64(
        headers,
        &[
            "anthropic-ratelimit-requests-limit",
            "x-ratelimit-limit-requests",
            "x-ratelimit-limit",
            "ratelimit-limit",
        ],
    );
    let now = unix_s();
    OkLikeState::from_remaining(remaining, limit, now).into()
}

struct OkLikeState(QuotaState);

impl OkLikeState {
    fn from_remaining(remaining: i64, limit: Option<i64>, now: i64) -> Self {
        Self(QuotaState {
            availability: if remaining == 0 {
                QuotaAvailability::Exhausted
            } else {
                QuotaAvailability::Available
            },
            remaining: Some(remaining),
            limit,
            reset_at_unix_s: None,
            observed_at_unix_s: Some(now),
            source: Some("quota_state_probe".to_string()),
        })
    }
}

impl From<OkLikeState> for Option<QuotaState> {
    fn from(value: OkLikeState) -> Self {
        Some(value.0)
    }
}

fn first_i64(headers: &dyn HeaderView, names: &[&str]) -> Option<i64> {
    names
        .iter()
        .find_map(|name| headers.get(name).and_then(|s| s.parse::<i64>().ok()))
}

fn availability_str(availability: QuotaAvailability) -> &'static str {
    match availability {
        QuotaAvailability::Unknown => "unknown",
        QuotaAvailability::Available => "available",
        QuotaAvailability::Exhausted => "exhausted",
    }
}

fn unix_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::{QuotaAvailability, QuotaState};

    #[test]
    fn update_sql_sets_columns_and_bumps_version() {
        let sql = update_sql();
        assert!(sql.contains("quota_availability = $2"));
        assert!(sql.contains("version = version + 1"));
        assert!(sql.contains("jsonb_set"));
    }

    #[test]
    fn exhausted_state_is_serializable_for_cache_writeback() {
        let state = QuotaState {
            availability: QuotaAvailability::Exhausted,
            remaining: Some(0),
            limit: Some(100),
            reset_at_unix_s: Some(1_717_200_030),
            observed_at_unix_s: Some(1_717_200_000),
            source: Some("response_header".to_string()),
        };
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value["availability"], "exhausted");
        assert_eq!(value["remaining"], 0);
    }
}
