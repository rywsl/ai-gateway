use gw_runtime::shutdown::{ShutdownCoordinator, ShutdownPhase};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn two_phase_drain_completes_clean() {
    let mut coord = ShutdownCoordinator::new();
    let token = coord.token();
    let done = Arc::new(AtomicUsize::new(0));

    for _ in 0..3 {
        let tok = token.clone();
        let d = done.clone();
        coord.register(tokio::spawn(async move {
            tok.cancelled().await;
            // Small work to simulate cleanup.
            tokio::time::sleep(Duration::from_millis(20)).await;
            d.fetch_add(1, Ordering::SeqCst);
        }));
    }

    let result = coord
        .drain(
            Duration::from_millis(50),
            Duration::from_millis(500),
            Duration::from_secs(2),
        )
        .await;

    assert_eq!(done.load(Ordering::SeqCst), 3);
    assert!(matches!(result, gw_runtime::shutdown::DrainOutcome::Clean));
}

#[tokio::test]
async fn phase_transitions_observable() {
    let mut coord = ShutdownCoordinator::new();
    let token = coord.token();
    let phase_handle = coord.phase_handle();

    coord.register(tokio::spawn(async move {
        tokio::select! {
            _ = token.cancelled() => {}
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }));

    assert_eq!(phase_handle.current(), ShutdownPhase::Running);
    let drain = tokio::spawn(async move {
        coord
            .drain(
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(500),
            )
            .await
    });

    // After a moment, we should be in Phase1 (accept-stop).
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(phase_handle.current() >= ShutdownPhase::Phase1AcceptStop);

    drain.await.unwrap();
    assert_eq!(phase_handle.current(), ShutdownPhase::Stopped);
}

#[tokio::test]
async fn hard_deadline_aborts_hung_task() {
    let mut coord = ShutdownCoordinator::new();
    let _token = coord.token();

    // A task that ignores cancellation and hangs.
    coord.register(tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }));

    let outcome = coord
        .drain(
            Duration::from_millis(20),
            Duration::from_millis(100),
            Duration::from_millis(200),
        )
        .await;

    assert!(matches!(outcome, gw_runtime::shutdown::DrainOutcome::Forced));
}

#[tokio::test]
async fn second_drain_call_emergency_aborts() {
    let mut coord = ShutdownCoordinator::new();
    let _token = coord.token();

    coord.register(tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }));

    // Trigger immediate emergency abort without waiting through phases.
    coord.emergency_abort();
    // Emergency abort sets phase to Stopped synchronously; outstanding
    // tasks are aborted.
    assert_eq!(
        coord.phase_handle().current(),
        gw_runtime::shutdown::ShutdownPhase::Stopped
    );
}
