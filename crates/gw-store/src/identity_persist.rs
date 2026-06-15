//! Lazy persist queue for VendorKey identity snapshots.
//!
//! Mirrors Aether's `_PENDING_LAZY_PERSIST` + `_PENDING_LAZY_PERSIST_LOCK`
//! pattern. Per-key dedup, total cap, fire-and-forget background writer.

use async_trait::async_trait;
use dashmap::DashSet;
use gw_core::impersonate::IdentitySnapshot;
use std::sync::Arc;

#[async_trait]
pub trait SnapshotWriter: Send + Sync + 'static {
    /// Write iff the DB column is NULL (no-op otherwise — preserves any
    /// admin-edited identity values).
    async fn write_if_null(&self, key_id: &str, snap: &IdentitySnapshot) -> anyhow::Result<()>;
}

pub struct IdentityPersistQueue {
    pending: Arc<DashSet<String>>,
    max_pending: usize,
    writer: Arc<dyn SnapshotWriter>,
}

impl IdentityPersistQueue {
    pub fn new(writer: Arc<dyn SnapshotWriter>, max_pending: usize) -> Self {
        Self {
            pending: Arc::new(DashSet::new()),
            max_pending,
            writer,
        }
    }

    /// Test helper — same as `new`, retained for clarity in tests.
    pub fn new_with_writer(writer: Arc<dyn SnapshotWriter>, max_pending: usize) -> Self {
        Self::new(writer, max_pending)
    }

    /// Returns `true` if the schedule was dropped (cap exceeded or duplicate
    /// — caller doesn't need to react, this is just for metrics).
    pub fn schedule(&self, key_id: String, snap: IdentitySnapshot) -> bool {
        if self.pending.len() >= self.max_pending {
            metrics::counter!("gw_egress_lazy_persist_total", "result" => "dropped").increment(1);
            return true;
        }
        if !self.pending.insert(key_id.clone()) {
            metrics::counter!("gw_egress_lazy_persist_total", "result" => "deferred").increment(1);
            return true;
        }
        let writer = Arc::clone(&self.writer);
        let pending = Arc::clone(&self.pending);
        tokio::spawn(async move {
            let result = writer.write_if_null(&key_id, &snap).await;
            match result {
                Ok(_) => {
                    metrics::counter!("gw_egress_lazy_persist_total", "result" => "ok").increment(1)
                }
                Err(e) => {
                    tracing::warn!("identity_persist failed key={key_id}: {e}");
                    metrics::counter!("gw_egress_lazy_persist_total", "result" => "failed")
                        .increment(1);
                }
            }
            pending.remove(&key_id);
        });
        false
    }

    /// Test helper — yields the runtime so spawned writes complete.
    pub async fn flush_for_test(&self) {
        tokio::task::yield_now().await;
        for _ in 0..16 {
            if self.pending.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    }
}

use sqlx::PgPool;

/// Conditional UPDATE used by [`PgSnapshotWriter`]. Writes `identity_snapshot`
/// only when the column is currently NULL, preserving admin-edited values.
/// `vendor_keys` primary key column is `id` (see migration 0001).
///
/// This is a cross-process contract surface (Go production exposes
/// `vendor_keys` as a writable VIEW over `provider_keys`; the VIEW must keep
/// `identity_snapshot` + `updated_at` writable). It is a `pub fn` so the
/// contract test can lock the column set against silent drift.
pub fn update_sql() -> &'static str {
    "UPDATE vendor_keys SET identity_snapshot = $1, updated_at = NOW() \
     WHERE id = $2 AND identity_snapshot IS NULL"
}

pub struct PgSnapshotWriter {
    pool: PgPool,
}

impl PgSnapshotWriter {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SnapshotWriter for PgSnapshotWriter {
    async fn write_if_null(&self, key_id: &str, snap: &IdentitySnapshot) -> anyhow::Result<()> {
        let payload = serde_json::to_value(snap)?;
        sqlx::query(update_sql())
            .bind(payload)
            .bind(key_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_sql_writes_identity_snapshot_only_when_null() {
        let sql = update_sql();
        assert!(sql.contains("UPDATE vendor_keys"));
        assert!(sql.contains("identity_snapshot = $1"));
        assert!(sql.contains("updated_at = NOW()"));
        // Conditional write preserves admin edits.
        assert!(sql.contains("WHERE id = $2 AND identity_snapshot IS NULL"));
    }
}
