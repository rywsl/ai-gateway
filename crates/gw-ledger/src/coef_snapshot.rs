//! Cold-load the latest complete coef table persisted by Go.
//!
//! Go still fans out live `coef_table` / `coef_table_commit` events per pod,
//! but a newly started pod needs a current table before it joins `gw:active_pods`.

use crate::coef::{apply_coef_commit, apply_coef_entry, CommitOutcome, Dim};
use anyhow::{anyhow, Context};
use fred::clients::RedisPool;
use fred::interfaces::HashesInterface;
use std::collections::HashMap;

pub const LATEST_COEF_ENTRIES_KEY: &str = "credit:coef_table:latest:entries";
pub const LATEST_COEF_META_KEY: &str = "credit:coef_table:latest:meta";

pub async fn load_latest_coef_snapshot(pool: &RedisPool) -> anyhow::Result<Option<CommitOutcome>> {
    let meta: HashMap<String, String> = pool
        .hgetall(LATEST_COEF_META_KEY)
        .await
        .context("read latest coef snapshot meta")?;
    if meta.is_empty() {
        return Ok(None);
    }

    let version = meta
        .get("version")
        .ok_or_else(|| anyhow!("latest coef snapshot missing version"))?
        .parse::<u64>()
        .context("parse latest coef snapshot version")?;
    let model_count = meta
        .get("model_count")
        .ok_or_else(|| anyhow!("latest coef snapshot missing model_count"))?
        .parse::<u32>()
        .context("parse latest coef snapshot model_count")?;

    let entries: HashMap<String, String> = pool
        .hgetall(LATEST_COEF_ENTRIES_KEY)
        .await
        .context("read latest coef snapshot entries")?;
    if entries.is_empty() {
        return Ok(None);
    }

    Ok(Some(apply_latest_coef_snapshot(
        version,
        model_count,
        entries,
    )))
}

pub fn apply_latest_coef_snapshot<I, K, V>(
    version: u64,
    model_count: u32,
    entries: I,
) -> CommitOutcome
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    for (field, coef_raw) in entries {
        let Some((model_id, dim_raw)) = field.as_ref().split_once('|') else {
            continue;
        };
        let Some(dim) = Dim::parse_str(dim_raw) else {
            continue;
        };
        let Ok(coef) = coef_raw.as_ref().parse::<u64>() else {
            continue;
        };
        apply_coef_entry(version, model_id, dim, coef);
    }
    apply_coef_commit(version, model_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coef::{current_snapshot, CoefSnapshot, COEF_CACHE, COEF_STAGING, TEST_COEF_LOCK};
    use std::sync::Arc;

    fn reset() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_COEF_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        COEF_CACHE.store(Arc::new(CoefSnapshot::empty()));
        COEF_STAGING.clear();
        guard
    }

    #[test]
    fn latest_snapshot_activates_complete_table() {
        let _guard = reset();
        let outcome = apply_latest_coef_snapshot(
            42,
            2,
            [
                ("model-a|input", "10"),
                ("model-a|output", "20"),
                ("model-b|input", "30"),
                ("model-b|cache_read", "40"),
            ],
        );

        assert_eq!(outcome, CommitOutcome::Activated);
        let snapshot = current_snapshot();
        assert_eq!(snapshot.version, 42);
        assert_eq!(snapshot.get("model-a").unwrap().input, 10);
        assert_eq!(snapshot.get("model-a").unwrap().output, 20);
        assert_eq!(snapshot.get("model-b").unwrap().cache_read, 40);
    }

    #[test]
    fn latest_snapshot_ignores_malformed_fields_and_reports_incomplete() {
        let _guard = reset();
        let outcome = apply_latest_coef_snapshot(
            43,
            2,
            [
                ("model-a|input", "10"),
                ("missing-separator", "20"),
                ("model-b|bogus", "30"),
                ("model-c|output", "not-a-number"),
            ],
        );

        assert_eq!(
            outcome,
            CommitOutcome::Incomplete {
                staged: 1,
                expected: 2
            }
        );
        assert_eq!(current_snapshot().version, 0);
    }
}
