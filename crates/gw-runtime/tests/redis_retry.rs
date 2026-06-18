use gw_runtime::redis_retry::{retry, retry_with_shutdown, RetryConfig, RetryError};
use gw_runtime::ShutdownToken;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

fn test_config(op_name: &str) -> RetryConfig {
    RetryConfig {
        op_name: op_name.to_string(),
        max_retries: 3,
        initial_backoff: Duration::from_millis(10),
        multipliers: vec![1.0, 2.0, 4.0],
        jitter: 0.0,
        total_deadline: Duration::from_secs(1),
    }
}

#[tokio::test]
async fn succeeds_on_first_attempt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let result: Result<i32, &str> = retry(&test_config("first_attempt"), || {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(42)
        }
    })
    .await;

    assert_eq!(result, Ok(42));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn retries_until_success() {
    let calls = Arc::new(AtomicUsize::new(0));
    let result: Result<i32, &str> = retry(&test_config("retry_success"), || {
        let calls = calls.clone();
        async move {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err("transient")
            } else {
                Ok(99)
            }
        }
    })
    .await;

    assert_eq!(result, Ok(99));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn exhausts_after_max_retries() {
    let calls = Arc::new(AtomicUsize::new(0));
    let result: Result<i32, &str> = retry(&test_config("exhaust"), || {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Err("always")
        }
    })
    .await;

    assert_eq!(result, Err("always"));
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn total_deadline_enforced() {
    let calls = Arc::new(AtomicUsize::new(0));
    let cfg = RetryConfig {
        max_retries: 10,
        initial_backoff: Duration::from_millis(200),
        multipliers: vec![1.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0],
        total_deadline: Duration::from_millis(300),
        ..test_config("deadline")
    };

    let start = Instant::now();
    let result: Result<i32, &str> = retry(&cfg, || {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Err("transient")
        }
    })
    .await;

    assert_eq!(result, Err("transient"));
    assert!(start.elapsed() <= Duration::from_millis(300));
    assert!(calls.load(Ordering::SeqCst) <= 3);
}

#[tokio::test(start_paused = true)]
async fn cancellation_interrupts_backoff_sleep() {
    let calls = Arc::new(AtomicUsize::new(0));
    let first_attempt = Arc::new(Notify::new());
    let cfg = RetryConfig {
        max_retries: 10,
        initial_backoff: Duration::from_secs(5),
        multipliers: vec![1.0],
        jitter: 0.0,
        total_deadline: Duration::from_secs(30),
        ..test_config("cancelled")
    };
    let shutdown = ShutdownToken::new();
    let shutdown_for_cancel = shutdown.clone();
    let first_attempt_for_cancel = first_attempt.clone();
    tokio::spawn(async move {
        first_attempt_for_cancel.notified().await;
        shutdown_for_cancel.cancel();
    });

    let result: Result<i32, RetryError<&str>> = retry_with_shutdown(&cfg, &shutdown, || {
        let calls = calls.clone();
        let first_attempt = first_attempt.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            first_attempt.notify_one();
            Err("transient")
        }
    })
    .await;

    assert_eq!(result, Err(RetryError::Cancelled));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
