use crate::metrics::{
    CREDIT_ACTIVE_REGISTER_ENQUEUE_TOTAL, CREDIT_ACTIVE_REGISTER_QUEUE_DEPTH,
    CREDIT_ACTIVE_REGISTER_REDIS_TOTAL,
};
use crate::ProtectionConfig;
use dashmap::mapref::entry::Entry;
use fred::clients::RedisPool;
use fred::interfaces::LuaInterface;
use gw_core::ids::UserId;
use gw_runtime::ShutdownToken;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub const ACTIVE_USERS_KEY: &str = "credit:active_users";
pub const ACTIVE_USER_PREFIX: &str = "credit:active_user:";

const ACTIVE_REGISTER_LUA: &str = r#"
redis.call('SADD', KEYS[1], ARGV[1])
redis.call('SET', KEYS[2], ARGV[2], 'EX', ARGV[3])
redis.call('EXPIRE', KEYS[1], ARGV[3])
return 1
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveRegistrationEnqueueResult {
    Queued,
    Throttled,
    Full,
    Closed,
    Disabled,
}

#[derive(Clone, Debug)]
struct ActiveRegistrationEvent {
    uid: UserId,
    last_seen_ms: u64,
    pod_id: String,
}

#[derive(Clone)]
pub struct ActiveRegistrationHandle {
    enabled: bool,
    pod_id: Arc<str>,
    min_interval_ms: u64,
    throttle_retention_ms: u64,
    queue_capacity: usize,
    last_prune_ms: Arc<AtomicU64>,
    last_enqueue_ms: Arc<dashmap::DashMap<UserId, u64>>,
    tx: Option<mpsc::Sender<ActiveRegistrationEvent>>,
}

pub fn active_user_key(uid: &UserId) -> String {
    format!("{ACTIVE_USER_PREFIX}{}", uid.as_ref())
}

impl ActiveRegistrationHandle {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            pod_id: Arc::from(""),
            min_interval_ms: 0,
            throttle_retention_ms: 0,
            queue_capacity: 0,
            last_prune_ms: Arc::new(AtomicU64::new(0)),
            last_enqueue_ms: Arc::new(dashmap::DashMap::new()),
            tx: None,
        }
    }

    #[cfg(test)]
    fn for_test(
        pod_id: &str,
        min_interval_ms: u64,
        capacity: usize,
    ) -> (Self, mpsc::Receiver<ActiveRegistrationEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                enabled: true,
                pod_id: Arc::from(pod_id),
                min_interval_ms,
                throttle_retention_ms: min_interval_ms.saturating_mul(2).max(1),
                queue_capacity: capacity,
                last_prune_ms: Arc::new(AtomicU64::new(0)),
                last_enqueue_ms: Arc::new(dashmap::DashMap::new()),
                tx: Some(tx),
            },
            rx,
        )
    }

    pub fn try_enqueue_activity(
        &self,
        uid: &UserId,
        admitted_at_ms: u64,
    ) -> ActiveRegistrationEnqueueResult {
        if !self.enabled {
            record_enqueue(ActiveRegistrationEnqueueResult::Disabled, None);
            return ActiveRegistrationEnqueueResult::Disabled;
        }
        let Some(tx) = &self.tx else {
            record_enqueue(ActiveRegistrationEnqueueResult::Closed, None);
            return ActiveRegistrationEnqueueResult::Closed;
        };

        if !self.reserve_enqueue(uid, admitted_at_ms) {
            record_enqueue(
                ActiveRegistrationEnqueueResult::Throttled,
                Some(self.queue_depth(tx)),
            );
            return ActiveRegistrationEnqueueResult::Throttled;
        }

        let event = ActiveRegistrationEvent {
            uid: uid.clone(),
            last_seen_ms: admitted_at_ms,
            pod_id: self.pod_id.to_string(),
        };
        match tx.try_send(event) {
            Ok(()) => {
                record_enqueue(
                    ActiveRegistrationEnqueueResult::Queued,
                    Some(self.queue_depth(tx)),
                );
                ActiveRegistrationEnqueueResult::Queued
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.rollback_reservation(uid, admitted_at_ms);
                record_enqueue(
                    ActiveRegistrationEnqueueResult::Full,
                    Some(self.queue_depth(tx)),
                );
                ActiveRegistrationEnqueueResult::Full
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.rollback_reservation(uid, admitted_at_ms);
                record_enqueue(ActiveRegistrationEnqueueResult::Closed, None);
                ActiveRegistrationEnqueueResult::Closed
            }
        }
    }

    fn reserve_enqueue(&self, uid: &UserId, admitted_at_ms: u64) -> bool {
        self.prune_old_throttle_entries(admitted_at_ms);
        match self.last_enqueue_ms.entry(uid.clone()) {
            Entry::Occupied(mut entry) => {
                if self.min_interval_ms > 0
                    && admitted_at_ms.saturating_sub(*entry.get()) < self.min_interval_ms
                {
                    false
                } else {
                    entry.insert(admitted_at_ms);
                    true
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(admitted_at_ms);
                true
            }
        }
    }

    fn prune_old_throttle_entries(&self, now_ms: u64) {
        let retention_ms = self.throttle_retention_ms;
        if retention_ms == 0 {
            return;
        }

        let last_prune_ms = self.last_prune_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last_prune_ms) < retention_ms {
            return;
        }
        if self
            .last_prune_ms
            .compare_exchange(last_prune_ms, now_ms, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        let cutoff_ms = now_ms.saturating_sub(retention_ms);
        self.last_enqueue_ms
            .retain(|_, last_seen_ms| *last_seen_ms > cutoff_ms);
    }

    fn rollback_reservation(&self, uid: &UserId, admitted_at_ms: u64) {
        self.last_enqueue_ms
            .remove_if(uid, |_, last| *last == admitted_at_ms);
    }

    fn queue_depth(&self, tx: &mpsc::Sender<ActiveRegistrationEvent>) -> usize {
        self.queue_capacity.saturating_sub(tx.capacity())
    }
}

pub fn spawn_active_registration_worker(
    redis: RedisPool,
    pod_id: String,
    cfg: &ProtectionConfig,
    shutdown: ShutdownToken,
) -> (ActiveRegistrationHandle, tokio::task::JoinHandle<()>) {
    if !cfg.enabled {
        return (ActiveRegistrationHandle::disabled(), tokio::spawn(async {}));
    }
    let capacity = cfg.active_registration_queue_capacity.max(1);
    let (tx, mut rx) = mpsc::channel(capacity);
    let handle = ActiveRegistrationHandle {
        enabled: true,
        pod_id: Arc::from(pod_id),
        min_interval_ms: cfg
            .active_registration_refresh_min_interval_secs
            .saturating_mul(1000),
        throttle_retention_ms: cfg
            .active_registration_ttl_secs
            .max(cfg.active_registration_refresh_min_interval_secs)
            .saturating_mul(1000)
            .max(1),
        queue_capacity: capacity,
        last_prune_ms: Arc::new(AtomicU64::new(0)),
        last_enqueue_ms: Arc::new(dashmap::DashMap::new()),
        tx: Some(tx),
    };
    let ttl_secs = cfg.active_registration_ttl_secs.max(1);
    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                maybe_event = rx.recv() => {
                    let Some(event) = maybe_event else { break };
                    if write_with_retry(&redis, &event, ttl_secs, &shutdown).await.is_err() {
                        metrics::counter!(
                            CREDIT_ACTIVE_REGISTER_REDIS_TOTAL,
                            "result" => "retry_exhausted"
                        ).increment(1);
                    }
                }
            }
        }
    });
    (handle, join)
}

async fn write_with_retry(
    redis: &RedisPool,
    event: &ActiveRegistrationEvent,
    ttl_secs: u64,
    shutdown: &ShutdownToken,
) -> Result<(), fred::error::RedisError> {
    let mut delay = Duration::from_millis(50);
    for attempt in 0..=4 {
        match write_registration_once(redis, event, ttl_secs).await {
            Ok(()) => {
                metrics::counter!(CREDIT_ACTIVE_REGISTER_REDIS_TOTAL, "result" => "success")
                    .increment(1);
                return Ok(());
            }
            Err(err) if attempt == 4 => return Err(err),
            Err(_) => {
                tokio::select! {
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(delay) => {
                        delay = (delay * 2).min(Duration::from_secs(1));
                    }
                }
            }
        }
    }
    Ok(())
}

async fn write_registration_once(
    redis: &RedisPool,
    event: &ActiveRegistrationEvent,
    ttl_secs: u64,
) -> Result<(), fred::error::RedisError> {
    let marker_key = active_user_key(&event.uid);
    let marker_value = format!("{}|{}", event.last_seen_ms, event.pod_id);
    let _: i64 = redis
        .eval(
            ACTIVE_REGISTER_LUA,
            vec![ACTIVE_USERS_KEY, marker_key.as_str()],
            vec![
                event.uid.as_ref(),
                marker_value.as_str(),
                &ttl_secs.to_string(),
            ],
        )
        .await?;
    Ok(())
}

fn record_enqueue(result: ActiveRegistrationEnqueueResult, queue_depth: Option<usize>) {
    let result_label = match result {
        ActiveRegistrationEnqueueResult::Queued => "queued",
        ActiveRegistrationEnqueueResult::Throttled => "throttled",
        ActiveRegistrationEnqueueResult::Full => "full",
        ActiveRegistrationEnqueueResult::Closed => "closed",
        ActiveRegistrationEnqueueResult::Disabled => "disabled",
    };
    metrics::counter!(
        CREDIT_ACTIVE_REGISTER_ENQUEUE_TOTAL,
        "result" => result_label
    )
    .increment(1);
    if let Some(queue_depth) = queue_depth {
        metrics::gauge!(CREDIT_ACTIVE_REGISTER_QUEUE_DEPTH).set(queue_depth as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fred::clients::RedisPool;
    use fred::interfaces::{ClientLike, KeysInterface, SetsInterface};
    use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};
    use gw_core::ids::UserId;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use testcontainers::{runners::AsyncRunner, ContainerAsync};
    use testcontainers_modules::redis::Redis;

    async fn redis_pool() -> (ContainerAsync<Redis>, RedisPool) {
        let container = Redis::default().start().await.expect("redis");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("redis port");
        let pool = RedisPool::new(
            RedisConfig::from_url(&format!("redis://127.0.0.1:{port}")).expect("redis config"),
            Some(PerformanceConfig::default()),
            Some(ConnectionConfig::default()),
            Some(ReconnectPolicy::new_constant(0, 100)),
            1,
        )
        .expect("redis pool");
        pool.init().await.expect("redis init");
        (container, pool)
    }

    fn uid(raw: &str) -> UserId {
        UserId::from(raw)
    }

    #[test]
    fn disabled_handle_never_queues() {
        let handle = ActiveRegistrationHandle::disabled();
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-disabled"), 1234),
            ActiveRegistrationEnqueueResult::Disabled
        );
    }

    #[test]
    fn handle_throttles_same_user_inside_interval() {
        let (handle, mut rx) = ActiveRegistrationHandle::for_test("pod-a", 30_000, 1);
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-a"), 10_000),
            ActiveRegistrationEnqueueResult::Queued
        );
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-a"), 15_000),
            ActiveRegistrationEnqueueResult::Throttled
        );
        let first = rx.try_recv().expect("first event");
        assert_eq!(first.uid.as_ref(), "user-a");
        assert_eq!(first.last_seen_ms, 10_000);
        assert_eq!(first.pod_id, "pod-a");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queue_full_is_non_fatal() {
        let (handle, _rx) = ActiveRegistrationHandle::for_test("pod-a", 0, 1);
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-a"), 10_000),
            ActiveRegistrationEnqueueResult::Queued
        );
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-b"), 10_001),
            ActiveRegistrationEnqueueResult::Full
        );
    }

    #[test]
    fn concurrent_same_user_activity_queues_once_inside_interval() {
        const THREADS: usize = 128;

        let (handle, mut rx) = ActiveRegistrationHandle::for_test("pod-a", 30_000, THREADS);
        let handle = Arc::new(handle);
        let barrier = Arc::new(Barrier::new(THREADS));
        let user = uid("user-concurrent");

        let threads = (0..THREADS)
            .map(|_| {
                let handle = Arc::clone(&handle);
                let barrier = Arc::clone(&barrier);
                let user = user.clone();
                thread::spawn(move || {
                    barrier.wait();
                    handle.try_enqueue_activity(&user, 10_000)
                })
            })
            .collect::<Vec<_>>();

        let results = threads
            .into_iter()
            .map(|thread| thread.join().expect("thread"))
            .collect::<Vec<_>>();
        let queued = results
            .iter()
            .filter(|result| **result == ActiveRegistrationEnqueueResult::Queued)
            .count();
        let throttled = results
            .iter()
            .filter(|result| **result == ActiveRegistrationEnqueueResult::Throttled)
            .count();

        assert_eq!(queued, 1, "results={results:?}");
        assert_eq!(throttled, THREADS - 1, "results={results:?}");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].uid.as_ref(), "user-concurrent");
        assert_eq!(events[0].last_seen_ms, 10_000);
    }

    #[test]
    fn full_queue_rolls_back_same_user_reservation() {
        let (handle, mut rx) = ActiveRegistrationHandle::for_test("pod-a", 30_000, 1);
        assert_eq!(
            handle.try_enqueue_activity(&uid("queued-user"), 10_000),
            ActiveRegistrationEnqueueResult::Queued
        );
        assert_eq!(
            handle.try_enqueue_activity(&uid("retry-user"), 10_001),
            ActiveRegistrationEnqueueResult::Full
        );

        let queued = rx.try_recv().expect("queued event");
        assert_eq!(queued.uid.as_ref(), "queued-user");

        assert_eq!(
            handle.try_enqueue_activity(&uid("retry-user"), 10_002),
            ActiveRegistrationEnqueueResult::Queued
        );
        let retry = rx.try_recv().expect("retry event");
        assert_eq!(retry.uid.as_ref(), "retry-user");
        assert_eq!(retry.last_seen_ms, 10_002);
    }

    #[test]
    fn old_throttle_entries_are_pruned_during_enqueue() {
        let (handle, mut rx) = ActiveRegistrationHandle::for_test("pod-a", 30_000, 8);

        assert_eq!(
            handle.try_enqueue_activity(&uid("old-user"), 10_000),
            ActiveRegistrationEnqueueResult::Queued
        );
        assert_eq!(
            handle.try_enqueue_activity(&uid("fresh-user"), 70_001),
            ActiveRegistrationEnqueueResult::Queued
        );

        let users = handle
            .last_enqueue_ms
            .iter()
            .map(|entry| entry.key().as_ref().to_string())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            users,
            std::collections::HashSet::from(["fresh-user".to_string()])
        );

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    #[ignore = "requires Docker (testcontainers Redis)"]
    async fn write_registration_sets_membership_marker_and_ttl() {
        let (_container, pool) = redis_pool().await;
        let event = ActiveRegistrationEvent {
            uid: uid("user-live"),
            last_seen_ms: 1_700_000_000_000,
            pod_id: "pod-1".to_string(),
        };

        write_registration_once(&pool, &event, 300)
            .await
            .expect("registration");

        let members: Vec<String> = pool.smembers(ACTIVE_USERS_KEY).await.expect("members");
        assert_eq!(members, vec!["user-live".to_string()]);
        let marker: String = pool
            .get(active_user_key(&event.uid))
            .await
            .expect("marker value");
        assert_eq!(marker, "1700000000000|pod-1");
        let marker_ttl: i64 = pool
            .ttl(active_user_key(&event.uid))
            .await
            .expect("marker ttl");
        let set_ttl: i64 = pool.ttl(ACTIVE_USERS_KEY).await.expect("set ttl");
        assert!((1..=300).contains(&marker_ttl), "marker ttl={marker_ttl}");
        assert!((1..=300).contains(&set_ttl), "set ttl={set_ttl}");
    }
}
