//! Stream consumer skeleton (ADR-007).
//!
//! On startup we create two consumer groups (`events:entity`, `events:counter`)
//! anchored at `$` (latest) so that we only see events published *after* the
//! pod started — combined with the cold-load chain this guarantees freshness.
//! Full `XREADGROUP` event loop is deferred; the loop currently sleeps so the
//! task handle stays alive for `gw-gateway` boot integration.

use super::entity_store::EntityStore;
use super::redis_client::RedisCtx;
use fred::interfaces::StreamsInterface;
use std::sync::Arc;
use tokio::task::JoinHandle;

pub struct StreamConsumer {
    pub redis: RedisCtx,
    pub store: Arc<EntityStore>,
    pub instance_id: String,
}

impl StreamConsumer {
    pub async fn start(self: Arc<Self>) -> anyhow::Result<JoinHandle<()>> {
        let client = self.redis.pool.next();
        // mkstream=true so the stream is created if missing; tolerate BUSYGROUP.
        let _: Result<(), _> = client
            .xgroup_create("events:entity", &*self.instance_id, "$", true)
            .await;
        let _: Result<(), _> = client
            .xgroup_create("events:counter", &*self.instance_id, "$", true)
            .await;
        let me = self.clone();
        let h = tokio::spawn(async move {
            me.consume_loop().await;
        });
        Ok(h)
    }

    async fn consume_loop(&self) {
        // Placeholder: XREADGROUP poll loop. Each frame should be applied to
        // `EntityStore` via `upsert_if_newer` semantics. Implementation is
        // deferred to a follow-up task; the loop keeps the task alive so the
        // gateway bootstrap can wait on the join handle.
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            tracing::trace!(
                target: "gw_store::stream_consumer",
                instance = %self.instance_id,
                "stream consumer idle tick (XREADGROUP not yet implemented)"
            );
        }
    }
}
