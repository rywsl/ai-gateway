//! InflightLedger — per-request HASH in Redis tracking the billing lifecycle.
//!
//! Spec §5.4. A key is written on `Sent` (`register`), updated on
//! `Settled` / `Terminated` (`mark_settled`), and removed after successful
//! UsageRecord emit (`clear`). The 24-hour `EXPIRE` is a safety net for
//! crashed processes whose explicit `clear` never ran; W4's startup
//! reconciler is the proper recovery path.

use fred::clients::RedisPool;
use fred::interfaces::{HashesInterface, KeysInterface};

/// Hash TTL in seconds — 24h safety net only. Crash recovery happens via the
/// W4 startup reconciler scanning these keys, not via TTL expiry.
const EXPIRE_SECS: i64 = 86_400;

#[derive(Clone, Debug)]
pub struct InflightRegistration {
    pub request_id: String,
    pub attempt_index: u32,
    pub user_id: String,
    pub vendor_key_id: String,
    pub freeze_amount: u64,
    pub prompt_estimate_tokens: u32,
}

#[derive(Clone, Copy, Debug)]
pub enum SettleState {
    Settled,
    Terminated,
}

impl SettleState {
    pub fn as_str(self) -> &'static str {
        match self {
            SettleState::Settled => "settled",
            SettleState::Terminated => "terminated",
        }
    }
}

#[derive(Clone)]
pub struct InflightLedger {
    pool: RedisPool,
    instance_id: String,
}

impl InflightLedger {
    pub fn new(pool: RedisPool, instance_id: String) -> Self {
        Self { pool, instance_id }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Compute the key for a given request / attempt. Public for tests;
    /// production callers should go through `register` / `clear`.
    pub fn test_key(instance_id: &str, request_id: &str, attempt_index: u32) -> String {
        format!(
            "billing:inflight:{}:{}:{}",
            instance_id, request_id, attempt_index
        )
    }

    fn key(&self, request_id: &str, attempt: u32) -> String {
        Self::test_key(&self.instance_id, request_id, attempt)
    }

    pub async fn register(&self, r: &InflightRegistration) -> anyhow::Result<()> {
        let key = self.key(&r.request_id, r.attempt_index);
        let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
        let fields: Vec<(&str, String)> = vec![
            ("state", "sent".to_string()),
            ("ts_sent_ms", now_ms.to_string()),
            ("user_id", r.user_id.clone()),
            ("vendor_key_id", r.vendor_key_id.clone()),
            ("freeze_amount", r.freeze_amount.to_string()),
            (
                "prompt_estimate_tokens",
                r.prompt_estimate_tokens.to_string(),
            ),
        ];
        let _: () = self.pool.hset::<(), _, _>(&key, fields).await?;
        let _: () = self.pool.expire::<(), _>(&key, EXPIRE_SECS).await?;
        Ok(())
    }

    pub async fn mark_settled(
        &self,
        request_id: &str,
        attempt: u32,
        state: SettleState,
    ) -> anyhow::Result<()> {
        let key = self.key(request_id, attempt);
        let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
        let fields: Vec<(&str, String)> = vec![
            ("state", state.as_str().to_string()),
            ("ts_settled_ms", now_ms.to_string()),
        ];
        let _: () = self.pool.hset::<(), _, _>(&key, fields).await?;
        Ok(())
    }

    pub async fn clear(&self, request_id: &str, attempt: u32) -> anyhow::Result<()> {
        let key = self.key(request_id, attempt);
        let _: () = self.pool.del::<(), _>(&[key]).await?;
        Ok(())
    }
}
