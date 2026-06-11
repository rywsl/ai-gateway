//! Integration test for resilience-fixes T2.2.
//!
//! Doesn't drive a real `run_with_signal_handler`: that would need Pingora,
//! Redis, PgSQL, and a real signal delivery — none of which fits a unit
//! test runner. Instead we exercise the *shape* of the signal-handler flow:
//! a `ShutdownCoordinator` registered with a `Supervisor`-managed task plus
//! a custom Future, with `drain` triggering a clean exit. This guards the
//! T2.2 contract (coordinator survives bootstrap; registered tasks see the
//! cancellation signal; drain returns `Clean` when all tasks exit within
//! their phase2 budget).
//!
//! T3 will wrap the actual gateway background spawns (Ledger flusher,
//! reaper, etc.) in `Supervisor` and register them with the coordinator
//! threaded out of `Wired.shutdown_token`. At that point the production
//! drain path exercises this same flow against real tasks.

use gw_runtime::{DrainOutcome, RestartPolicy, ShutdownCoordinator, Supervisor};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

/// Custom Future that increments a counter when polled and resolves when
/// the inner `ShutdownToken` is cancelled. Demonstrates that any task
/// shape — not just `tokio::spawn`-friendly async fns — can participate in
/// the drain as long as it observes the cancellation token.
struct WatchingFuture {
    polls: Arc<AtomicU32>,
    token: gw_runtime::ShutdownToken,
    inner: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl WatchingFuture {
    fn new(polls: Arc<AtomicU32>, token: gw_runtime::ShutdownToken) -> Self {
        let inner_token = token.clone();
        let inner: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            inner_token.cancelled().await;
        });
        Self {
            polls,
            token,
            inner,
        }
    }
}

impl Future for WatchingFuture {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if self.token.is_cancelled() {
            return Poll::Ready(());
        }
        self.inner.as_mut().poll(cx)
    }
}

#[tokio::test]
async fn coordinator_drains_supervisor_task_and_custom_future() {
    let mut coord = ShutdownCoordinator::new();
    let token = coord.token();

    // Supervised task: increments a counter on each restart attempt, then
    // parks on the inherited shutdown token until drain cancels it.
    let supervisor_runs = Arc::new(AtomicU32::new(0));
    let s = supervisor_runs.clone();
    let sup_handle = Supervisor::new("test-supervised", RestartPolicy::default(), token.clone())
        .spawn(move |tok| {
            let s = s.clone();
            async move {
                s.fetch_add(1, Ordering::SeqCst);
                tok.cancelled().await;
                Ok(())
            }
        });
    coord.register(sup_handle);

    // Custom future spawned as a tokio task. Exercises the "any
    // JoinHandle<()> can be registered" contract.
    let polls = Arc::new(AtomicU32::new(0));
    let watch = WatchingFuture::new(polls.clone(), token.clone());
    let watch_handle = tokio::spawn(watch);
    coord.register(watch_handle);

    // Give both tasks a moment to enter their waits.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        supervisor_runs.load(Ordering::SeqCst) >= 1,
        "supervised task must run at least once before drain"
    );

    let outcome = coord
        .drain(
            Duration::from_millis(50),
            Duration::from_millis(500),
            Duration::from_secs(2),
        )
        .await;

    assert_eq!(
        outcome,
        DrainOutcome::Clean,
        "all registered tasks observed cancellation and exited within phase2"
    );
    assert!(
        polls.load(Ordering::SeqCst) >= 1,
        "custom future must have been polled"
    );
}

#[tokio::test]
async fn second_signal_path_emergency_aborts_hung_task() {
    // The production handler does `std::process::exit(1)` on a second
    // signal; in-test we verify the underlying `emergency_abort` mechanic
    // unsticks an otherwise unresponsive task.
    let mut coord = ShutdownCoordinator::new();
    let _token = coord.token();

    // Task that ignores the cancellation token — simulating a hung
    // background loop.
    coord.register(tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }));

    coord.emergency_abort();
    assert_eq!(
        coord.phase_handle().current(),
        gw_runtime::ShutdownPhase::Stopped,
        "emergency_abort must drive phase straight to Stopped"
    );
}
