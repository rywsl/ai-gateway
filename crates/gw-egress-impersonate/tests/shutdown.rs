//! Regression test for Bug 2: EgressHandle::shutdown must return promptly
//! (well under drain_timeout) when no requests are inflight.
//!
//! Previously, `shutdown` polled `broadcast::Sender::receiver_count()` to
//! detect supervisor exit, but the supervisor held a receiver for the entire
//! drain — so the loop always spun to `drain_timeout`. This test verifies the
//! fix: a `watch::Receiver<bool>` that the supervisor flips on exit.

use gw_egress_impersonate::handle::{EgressBind, EgressHandle};
use std::time::Duration;

/// Shutdown without inflight requests must complete well under drain_timeout.
///
/// We give `drain_timeout = 5s` and assert the call returns in under 2s.
/// On a healthy system this should take <100 ms; the 2s ceiling gives plenty
/// of CI headroom while still catching the "spin to timeout" regression.
#[tokio::test]
async fn egress_handle_shutdown_returns_promptly() {
    // Use port 0 so the OS picks a free loopback port — no tempdir needed.
    let bind = EgressBind::Tcp("127.0.0.1:0".parse().expect("valid addr"));
    let handle = EgressHandle::start(bind, 4);

    // Give the supervisor a moment to bind and enter its accept loop.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let start = std::time::Instant::now();
    handle.shutdown(Duration::from_secs(5)).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "shutdown took {elapsed:?}, expected < 2s — likely spinning to drain_timeout"
    );
}
