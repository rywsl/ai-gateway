//! `events:entity` invalidation emitter.
//!
//! After the fake control-plane upserts or soft-deletes an entity row, it
//! XADDs `{ns, id}` to `events:entity`. The gateway's stream consumer
//! switches on a fixed `ns` vocabulary; see the table->ns map in
//! [`crate::store::ns_for_table`].

use fred::clients::RedisPool;
use fred::interfaces::StreamsInterface;
use std::sync::Arc;

const ENTITY_STREAM: &str = "events:entity";

/// XADD an invalidation event with two fields `(ns, id)` to `events:entity`.
///
/// The auto-generated stream id is returned by Redis but discarded here —
/// the gateway consumer only reads forward via XREADGROUP.
pub async fn emit_invalidate(redis: &Arc<RedisPool>, ns: &str, id: &str) -> anyhow::Result<()> {
    let client = redis.next();
    let _: String = client
        .xadd(
            ENTITY_STREAM,
            false,
            None::<()>,
            "*",
            vec![("ns", ns.to_string()), ("id", id.to_string())],
        )
        .await?;
    Ok(())
}
