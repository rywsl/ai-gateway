use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, HashesInterface, KeysInterface, LuaInterface, SortedSetsInterface};
use fred::types::{ConnectionConfig, MultipleZaddValues, PerformanceConfig, ReconnectPolicy, RedisConfig};
use gw_core::ids::UserId;
use gw_task_runtime::{ReleaseOutcome, RedisTaskFreezeRelease, TaskFreezeRelease, TASK_FREEZE_RELEASE_LUA};
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

async fn make_pool(url: &str) -> RedisPool {
    let cfg = RedisConfig::from_url(url).expect("parse redis url");
    let pool = RedisPool::new(
        cfg,
        Some(PerformanceConfig::default()),
        Some(ConnectionConfig::default()),
        Some(ReconnectPolicy::default()),
        1,
    )
    .expect("RedisPool");
    pool.init().await.expect("connect");
    pool
}

fn keys(uid: &UserId, task_id: &str) -> (String, String, String) {
    let tag = format!("{{user:{}}}", uid.as_ref());
    (
        format!("{tag}:tasks:leases"),
        format!("{tag}:entity:task:{task_id}"),
        format!("{tag}:counter:task_frozen_micro"),
    )
}

#[test]
fn lua_script_contains_single_atomic_release_sequence() {
    let script = TASK_FREEZE_RELEASE_LUA;
    assert!(script.contains("redis.call('ZREM'"));
    assert!(script.contains("return 'already_released'"));
    assert!(script.contains("redis.call('HSET'"));
    assert!(script.contains("redis.call('HINCRBY'"));
    assert!(script.contains("return 'ok'"));
    assert_eq!(script.matches("redis.call(").count(), 3);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn normal_release_updates_lease_status_and_counter_atomically() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let pool = make_pool(&format!("redis://127.0.0.1:{port}")).await;
    let releaser = RedisTaskFreezeRelease::new(pool.clone()).await.unwrap();

    let uid = UserId::from("release-normal");
    let task_id = "task-normal";
    let (leases_key, task_key, counter_key) = keys(&uid, task_id);

    let _: () = pool
        .next()
        .zadd(&leases_key, None, None, false, false, MultipleZaddValues::try_from((1_f64, task_id)).unwrap())
        .await
        .unwrap();
    let _: () = pool.next().hset(&task_key, ("status", "running")).await.unwrap();
    let _: () = pool.next().hset(&counter_key, ("frozen_micro", 5_000_i64)).await.unwrap();

    let outcome = releaser.release(&uid, task_id, 1_000).await.unwrap();
    assert_eq!(outcome, ReleaseOutcome::Released);

    let score: Option<i64> = pool.next().zscore(&leases_key, task_id).await.unwrap();
    let status: String = pool.next().hget(&task_key, "status").await.unwrap();
    let counter: i64 = pool.next().hget(&counter_key, "frozen_micro").await.unwrap();
    assert_eq!(score, None);
    assert_eq!(status, "released");
    assert_eq!(counter, 4_000);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn double_release_is_idempotent_and_counter_changes_once() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let pool = make_pool(&format!("redis://127.0.0.1:{port}")).await;
    let releaser = RedisTaskFreezeRelease::new(pool.clone()).await.unwrap();

    let uid = UserId::from("release-double");
    let task_id = "task-double";
    let (leases_key, task_key, counter_key) = keys(&uid, task_id);

    let _: () = pool
        .next()
        .zadd(&leases_key, None, None, false, false, MultipleZaddValues::try_from((1_f64, task_id)).unwrap())
        .await
        .unwrap();
    let _: () = pool.next().hset(&task_key, ("status", "running")).await.unwrap();
    let _: () = pool.next().hset(&counter_key, ("frozen_micro", 5_000_i64)).await.unwrap();

    assert_eq!(
        releaser.release(&uid, task_id, 1_000).await.unwrap(),
        ReleaseOutcome::Released
    );
    assert_eq!(
        releaser.release(&uid, task_id, 1_000).await.unwrap(),
        ReleaseOutcome::AlreadyReleased
    );

    let counter: i64 = pool.next().hget(&counter_key, "frozen_micro").await.unwrap();
    assert_eq!(counter, 4_000);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn unknown_task_returns_already_released_and_leaves_counter_unchanged() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let pool = make_pool(&format!("redis://127.0.0.1:{port}")).await;
    let releaser = RedisTaskFreezeRelease::new(pool.clone()).await.unwrap();

    let uid = UserId::from("release-unknown");
    let task_id = "task-unknown";
    let (_, _, counter_key) = keys(&uid, task_id);
    let _: () = pool.next().hset(&counter_key, ("frozen_micro", 5_000_i64)).await.unwrap();

    let outcome = releaser.release(&uid, task_id, 1_000).await.unwrap();
    assert_eq!(outcome, ReleaseOutcome::AlreadyReleased);

    let counter: i64 = pool.next().hget(&counter_key, "frozen_micro").await.unwrap();
    assert_eq!(counter, 5_000);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn script_executes_as_one_eval_for_all_or_nothing_release() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let pool = make_pool(&format!("redis://127.0.0.1:{port}")).await;

    let uid = UserId::from("release-atomicity");
    let task_id = "task-atomicity";
    let (leases_key, task_key, counter_key) = keys(&uid, task_id);
    let wrong_type_counter_key = format!("{counter_key}:wrong-type");

    let _: () = pool
        .next()
        .zadd(&leases_key, None, None, false, false, MultipleZaddValues::try_from((1_f64, task_id)).unwrap())
        .await
        .unwrap();
    let _: () = pool.next().hset(&task_key, ("status", "running")).await.unwrap();
    let _: () = pool.next().set(&wrong_type_counter_key, "not-a-hash", None, None, false).await.unwrap();

    let result: Result<String, _> = pool
        .next()
        .eval(
            TASK_FREEZE_RELEASE_LUA,
            vec![leases_key.as_str(), task_key.as_str(), wrong_type_counter_key.as_str()],
            vec![task_id, "1000"],
        )
        .await;
    assert!(result.is_err());

    let score: Option<i64> = pool.next().zscore(&leases_key, task_id).await.unwrap();
    let status: String = pool.next().hget(&task_key, "status").await.unwrap();
    assert_eq!(score, Some(1));
    assert_eq!(status, "running");
}
