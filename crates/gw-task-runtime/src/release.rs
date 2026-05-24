use crate::{ReleaseOutcome, TaskFreezeRelease, UserId};
use anyhow::{anyhow, Context};
use fred::clients::RedisPool;
use fred::error::RedisError;
use fred::interfaces::LuaInterface;

pub const TASK_FREEZE_RELEASE_LUA: &str = include_str!("lua/task_freeze_release.lua");

/// Redis-backed implementation of [`TaskFreezeRelease`].
///
/// The release operation is a single Lua script, keeping lease removal,
/// task status mutation, and frozen-counter decrement atomic. All keys share
/// a `{user:<uid>}` hash tag so Redis Cluster places them in one slot.
#[derive(Clone)]
pub struct RedisTaskFreezeRelease {
    pool: RedisPool,
    script_sha: String,
}

impl RedisTaskFreezeRelease {
    pub async fn new(pool: RedisPool) -> anyhow::Result<Self> {
        let script_sha: String = pool
            .next()
            .script_load(TASK_FREEZE_RELEASE_LUA)
            .await
            .context("load task freeze release lua")?;
        Ok(Self { pool, script_sha })
    }

    pub fn from_loaded_sha(pool: RedisPool, script_sha: impl Into<String>) -> Self {
        Self {
            pool,
            script_sha: script_sha.into(),
        }
    }
}

#[async_trait::async_trait]
impl TaskFreezeRelease for RedisTaskFreezeRelease {
    async fn release(
        &self,
        uid: &UserId,
        task_id: &str,
        frozen_amount: u64,
    ) -> Result<ReleaseOutcome, anyhow::Error> {
        let tag = format!("{{user:{}}}", uid.as_ref());
        let leases_key = format!("{tag}:tasks:leases");
        let task_key = format!("{tag}:entity:task:{task_id}");
        let counter_key = format!("{tag}:counter:task_frozen_micro");
        let amount = frozen_amount.to_string();

        let client = self.pool.next();
        let keys = vec![leases_key.as_str(), task_key.as_str(), counter_key.as_str()];
        let args = vec![task_id, amount.as_str()];

        let raw = match client
            .evalsha::<String, _, _, _>(self.script_sha.as_str(), keys.clone(), args.clone())
            .await
        {
            Ok(raw) => raw,
            Err(err) if is_no_script(&err) => client
                .eval::<String, _, _, _>(TASK_FREEZE_RELEASE_LUA, keys, args)
                .await
                .context("eval task freeze release lua after NOSCRIPT")?,
            Err(err) => return Err(err).context("evalsha task freeze release lua"),
        };

        match raw.as_str() {
            "ok" => Ok(ReleaseOutcome::Released),
            "already_released" => Ok(ReleaseOutcome::AlreadyReleased),
            other => Err(anyhow!("unexpected task freeze release lua result: {other}")),
        }
    }
}

fn is_no_script(err: &RedisError) -> bool {
    err.to_string().contains("NOSCRIPT")
}
