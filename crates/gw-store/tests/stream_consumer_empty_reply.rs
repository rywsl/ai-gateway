//! StreamConsumer must NOT log a parse error when both streams are empty.
//! Requires Docker.

use gw_store::redis_client::RedisCtx;
use gw_store::stream_consumer::StreamConsumer;
use gw_store::EntityStore;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

#[derive(Default)]
struct WarnCounter {
    seen: Arc<AtomicUsize>,
}

impl<S: Subscriber> Layer<S> for WarnCounter {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().level() == &tracing::Level::WARN
            && event.metadata().target() == "gw_store::stream_consumer"
        {
            self.seen.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn empty_streams_produce_zero_warn_logs_over_1s() {
    let counter = WarnCounter::default();
    let seen = counter.seen.clone();
    let subscriber = tracing_subscriber::registry().with(counter);
    let _guard = tracing::subscriber::set_default(subscriber);

    let redis_node = Redis::default().start().await.unwrap();
    let redis_port = redis_node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{redis_port}"), 4)
        .await
        .unwrap();

    let store = Arc::new(EntityStore::new());
    let consumer = Arc::new(StreamConsumer {
        redis,
        store,
        error_message_rules: None,
        instance_id: "test-empty-reply".into(),
    });
    let _h = consumer.start();

    // Give the consumer ~6 poll cycles (each BLOCK_MS=200) without XADDs.
    tokio::time::sleep(Duration::from_millis(1_200)).await;

    let warn_count = seen.load(Ordering::Relaxed);
    assert_eq!(
        warn_count, 0,
        "expected zero warn logs from gw_store::stream_consumer over 1.2s, got {warn_count}"
    );
}
