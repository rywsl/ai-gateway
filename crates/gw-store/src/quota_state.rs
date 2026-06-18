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
}

impl QuotaStateWriter {
    pub fn new(pg: PgPool) -> Self {
        Self { pg }
    }

    pub async fn write(&self, key_id: &str, state: &QuotaState) -> anyhow::Result<()> {
        let mut tx = self.pg.begin().await?;
        sqlx::query(update_sql())
            .bind(key_id)
            .bind(availability_str(state.availability))
            .bind(state.remaining)
            .bind(state.limit)
            .bind(state.reset_at_unix_s)
            .bind(state.observed_at_unix_s)
            .execute(&mut *tx)
            .await?;

        sqlx::query(insert_outbox_sql())
            .bind(key_id)
            .bind(serde_json::to_value(state)?)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(())
    }
}

pub struct QuotaStateOutboxProcessor {
    pg: PgPool,
    redis: RedisCtx,
}

#[derive(Debug)]
struct QuotaStateOutboxRow {
    id: i64,
    vendor_key_id: String,
    status: String,
    attempt: i32,
}

impl QuotaStateOutboxProcessor {
    pub fn new(pg: PgPool, redis: RedisCtx) -> Self {
        Self { pg, redis }
    }

    pub async fn process_batch(&self, batch_size: i64) -> anyhow::Result<u64> {
        let mut tx = self.pg.begin().await?;
        let rows = sqlx::query_as::<_, (i64, String, String, i32)>(select_due_outbox_sql())
            .bind(batch_size)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|(id, vendor_key_id, status, attempt)| QuotaStateOutboxRow {
                id,
                vendor_key_id,
                status,
                attempt,
            })
            .collect::<Vec<_>>();

        let mut processed = 0_u64;
        for row in &rows {
            match self.process_row(&mut tx, row).await {
                Ok(()) => {
                    processed += 1;
                }
                Err(err) => {
                    tracing::warn!(
                        target: "gw_store::quota_state",
                        outbox_id = row.id,
                        vendor_key_id = %row.vendor_key_id,
                        error = %err,
                        "quota state outbox row retry scheduled"
                    );
                    mark_outbox_retry(&mut tx, row.id, row.attempt).await?;
                }
            }
        }
        tx.commit().await?;
        Ok(processed)
    }

    async fn process_row(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        row: &QuotaStateOutboxRow,
    ) -> anyhow::Result<()> {
        match row.status.as_str() {
            "pending" => {
                self.emit_entity_event(&row.vendor_key_id).await?;
                set_outbox_status(tx, row.id, "emitted_xadd").await?;
                self.delete_vendor_key_cache(&row.vendor_key_id).await?;
                set_outbox_status(tx, row.id, "emitted_del").await?;
                set_outbox_committed(tx, row.id).await?;
            }
            "emitted_xadd" => {
                self.delete_vendor_key_cache(&row.vendor_key_id).await?;
                set_outbox_status(tx, row.id, "emitted_del").await?;
                set_outbox_committed(tx, row.id).await?;
            }
            "emitted_del" => {
                set_outbox_committed(tx, row.id).await?;
            }
            other => anyhow::bail!("unsupported quota_state_outbox status {other}"),
        }
        Ok(())
    }

    async fn emit_entity_event(&self, key_id: &str) -> anyhow::Result<()> {
        let client = self.redis.pool.next();
        client
            .xadd::<String, _, _, _, _>(
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
            .await?;
        Ok(())
    }

    async fn delete_vendor_key_cache(&self, key_id: &str) -> anyhow::Result<()> {
        let client = self.redis.pool.next();
        let redis_key = format!("entity:vendor_key:{key_id}");
        client.del::<(), _>(redis_key).await?;
        Ok(())
    }
}

pub fn insert_outbox_sql() -> &'static str {
    "INSERT INTO quota_state_outbox (vendor_key_id, payload, status) \
     VALUES ($1, $2, 'pending')"
}

pub fn select_due_outbox_sql() -> &'static str {
    "SELECT id, vendor_key_id, status, attempt \
     FROM quota_state_outbox \
     WHERE status IN ('pending', 'emitted_xadd', 'emitted_del') \
       AND next_attempt_at <= NOW() \
     ORDER BY id ASC \
     LIMIT $1 \
     FOR UPDATE SKIP LOCKED"
}

fn set_outbox_status_sql() -> &'static str {
    "UPDATE quota_state_outbox \
     SET status = $2, updated_at = NOW() \
     WHERE id = $1"
}

fn set_outbox_committed_sql() -> &'static str {
    "UPDATE quota_state_outbox \
     SET status = 'committed', updated_at = NOW() \
     WHERE id = $1"
}

async fn set_outbox_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: i64,
    status: &str,
) -> anyhow::Result<()> {
    sqlx::query(set_outbox_status_sql())
        .bind(id)
        .bind(status)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn set_outbox_committed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: i64,
) -> anyhow::Result<()> {
    sqlx::query(set_outbox_committed_sql())
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn mark_outbox_retry(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: i64,
    attempt: i32,
) -> anyhow::Result<()> {
    let next_attempt = attempt.saturating_add(1);
    if next_attempt >= 5 {
        sqlx::query(
            "UPDATE quota_state_outbox \
             SET status = 'failed', attempt = $2, updated_at = NOW() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(next_attempt)
        .execute(&mut **tx)
        .await?;
    } else {
        let bounded_attempt = next_attempt.clamp(0, 5) as u32;
        let delay_secs = i64::from(1_i32 << bounded_attempt);
        sqlx::query(
            "UPDATE quota_state_outbox \
             SET attempt = $2, \
                 next_attempt_at = NOW() + ($3::TEXT || ' seconds')::INTERVAL, \
                 updated_at = NOW() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(next_attempt)
        .bind(delay_secs.to_string())
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
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
    fn quota_state_writer_persists_outbox_in_pg_transaction() {
        let src = include_str!("quota_state.rs");
        let production_src = src
            .split("#[cfg(test)]")
            .next()
            .expect("production section");

        assert!(
            production_src.contains("INSERT INTO quota_state_outbox"),
            "writer must persist a quota_state_outbox row instead of best-effort Redis"
        );
        assert!(
            production_src.contains("tx.commit().await"),
            "vendor_keys update and outbox insert must commit together"
        );
        assert!(
            !production_src.contains("let _: Result<(), _> = client.del"),
            "writer must not ignore Redis DEL failures after PG commit"
        );
        assert!(
            !production_src.contains("let _: Result<String, _> = client"),
            "writer must not ignore Redis XADD failures after PG commit"
        );
    }

    #[test]
    fn quota_state_outbox_processor_uses_locked_due_rows_and_state_steps() {
        let src = include_str!("quota_state.rs");
        let production_src = src
            .split("#[cfg(test)]")
            .next()
            .expect("production section");

        assert!(production_src.contains("FOR UPDATE SKIP LOCKED"));
        assert!(production_src.contains("'pending'"));
        assert!(production_src.contains("'emitted_xadd'"));
        assert!(production_src.contains("'committed'"));
        assert!(production_src.contains("status = 'failed'"));
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
