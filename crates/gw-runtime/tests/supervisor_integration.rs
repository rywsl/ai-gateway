use gw_runtime::shutdown::ShutdownToken;
use gw_runtime::supervisor::{RestartPolicy, Supervisor};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn fast_policy() -> RestartPolicy {
    RestartPolicy {
        max_restarts: 3,
        window: Duration::from_secs(1),
        backoff_initial: Duration::from_millis(10),
        backoff_max: Duration::from_millis(50),
        backoff_multiplier: 2.0,
        backoff_jitter: 0.0,
    }
}

#[tokio::test]
async fn restarts_on_returned_error() {
    let counter = Arc::new(AtomicU32::new(0));
    let token = ShutdownToken::new();
    let c = counter.clone();
    let handle = Supervisor::new("test", fast_policy(), token.clone())
        .spawn(move |tok| {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(anyhow::anyhow!("boom"))
                } else {
                    // Stay alive until shutdown.
                    tok.cancelled().await;
                    Ok(())
                }
            }
        });

    tokio::time::sleep(Duration::from_millis(300)).await;
    token.cancel();
    handle.await.unwrap();
    assert!(counter.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn restarts_on_panic() {
    let counter = Arc::new(AtomicU32::new(0));
    let token = ShutdownToken::new();
    let c = counter.clone();
    let handle = Supervisor::new("panicker", fast_policy(), token.clone())
        .spawn(move |tok| {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    panic!("crash");
                }
                tok.cancelled().await;
                Ok(())
            }
        });

    tokio::time::sleep(Duration::from_millis(200)).await;
    token.cancel();
    handle.await.unwrap();
    assert!(counter.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn marks_unhealthy_after_max_restarts() {
    let counter = Arc::new(AtomicU32::new(0));
    let token = ShutdownToken::new();
    let policy = RestartPolicy {
        max_restarts: 2,
        window: Duration::from_secs(10),
        ..fast_policy()
    };
    let c = counter.clone();
    let sup = Supervisor::new("flaky", policy, token.clone());
    let unhealthy = sup.unhealthy_flag();
    let handle = sup.spawn(move |_| {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("always fail"))
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;
    handle.await.unwrap();
    assert!(unhealthy.load(Ordering::SeqCst));
    // Exactly max_restarts + 1 = 3 runs before giving up.
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn shutdown_stops_restarts() {
    let counter = Arc::new(AtomicU32::new(0));
    let token = ShutdownToken::new();
    let c = counter.clone();
    let handle = Supervisor::new("test", fast_policy(), token.clone())
        .spawn(move |_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("err"))
            }
        });

    tokio::time::sleep(Duration::from_millis(50)).await;
    token.cancel();
    handle.await.unwrap();
    let final_count = counter.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        counter.load(Ordering::SeqCst),
        final_count,
        "no restart after shutdown"
    );
}

#[tokio::test]
async fn restarts_on_panic_during_factory_construction() {
    // If the factory closure panics before returning the future at all,
    // the supervisor must still catch it (it's wrapped in catch_unwind)
    // and treat it as a failed run that counts toward the restart budget.
    let counter = Arc::new(AtomicU32::new(0));
    let token = ShutdownToken::new();
    let policy = RestartPolicy {
        max_restarts: 2,
        window: Duration::from_secs(10),
        ..fast_policy()
    };
    let c = counter.clone();
    let sup = Supervisor::new("factory_panicker", policy, token.clone());
    let unhealthy = sup.unhealthy_flag();
    let handle = sup.spawn(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
        // Panic BEFORE returning the async block. Without the fix the
        // supervisor task would itself unwind and the JoinHandle would
        // resolve to Err(JoinError::panic), failing handle.await.unwrap().
        panic!("factory boom");
        #[allow(unreachable_code)]
        async {
            Ok(())
        }
    });

    handle.await.unwrap();
    assert!(unhealthy.load(Ordering::SeqCst));
    // max_restarts + 1 = 3 attempts before giving up.
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

