//! Two-phase shutdown coordinator.
//!
//! Phase 0 (Running) → Phase 1 (AcceptStop) → Phase 2 (TaskDrain) → Stopped.
//!
//! Phase 1 flips `/readyz` to 503 so external load balancers (k8s) stop
//! sending new traffic. Pingora `shutdown_after` should be wired to the
//! same token so it stops accepting new connections.
//!
//! Phase 2 waits for every `register`-ed JoinHandle to complete. After
//! `hard_deadline` it `abort()`s outstanding tasks and returns `Forced`.

use arc_swap::ArcSwap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ShutdownPhase {
    Running = 0,
    Phase1AcceptStop = 1,
    Phase2TaskDrain = 2,
    Stopped = 3,
}

impl ShutdownPhase {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Running,
            1 => Self::Phase1AcceptStop,
            2 => Self::Phase2TaskDrain,
            _ => Self::Stopped,
        }
    }
}

#[derive(Clone, Default)]
pub struct ShutdownToken {
    inner: CancellationToken,
}

impl ShutdownToken {
    pub fn new() -> Self {
        Self {
            inner: CancellationToken::new(),
        }
    }

    pub fn child(&self) -> Self {
        Self {
            inner: self.inner.child_token(),
        }
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.inner.cancelled().await;
    }
}

#[derive(Clone)]
pub struct PhaseHandle {
    state: Arc<AtomicU8>,
}

impl PhaseHandle {
    pub fn current(&self) -> ShutdownPhase {
        ShutdownPhase::from_u8(self.state.load(Ordering::Acquire))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DrainOutcome {
    Clean,
    Forced,
}

pub struct ShutdownCoordinator {
    token: ShutdownToken,
    handles: Vec<JoinHandle<()>>,
    phase: Arc<AtomicU8>,
    // Hold an ArcSwap for additional shutdown listeners that need a clone
    // of the handle but can also be appended after construction.
    _phase_swap: Arc<ArcSwap<ShutdownPhase>>,
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self {
            token: ShutdownToken::new(),
            handles: Vec::new(),
            phase: Arc::new(AtomicU8::new(ShutdownPhase::Running as u8)),
            _phase_swap: Arc::new(ArcSwap::from_pointee(ShutdownPhase::Running)),
        }
    }

    pub fn token(&self) -> ShutdownToken {
        self.token.clone()
    }

    pub fn phase_handle(&self) -> PhaseHandle {
        PhaseHandle {
            state: self.phase.clone(),
        }
    }

    pub fn register(&mut self, h: JoinHandle<()>) {
        self.handles.push(h);
    }

    pub async fn drain(
        mut self,
        phase1: Duration,
        phase2: Duration,
        hard_deadline: Duration,
    ) -> DrainOutcome {
        let start = Instant::now();

        // Phase 1 — stop accepting new work.
        self.phase
            .store(ShutdownPhase::Phase1AcceptStop as u8, Ordering::Release);
        metrics::gauge!("gw_shutdown_phase").set(1.0);
        self.token.cancel();
        tokio::time::sleep(phase1).await;

        // Phase 2 — wait for registered tasks to exit.
        self.phase
            .store(ShutdownPhase::Phase2TaskDrain as u8, Ordering::Release);
        metrics::gauge!("gw_shutdown_phase").set(2.0);

        let phase2_deadline = Instant::now() + phase2;
        let mut any_forced = false;
        for h in std::mem::take(&mut self.handles) {
            let remaining = phase2_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                h.abort();
                any_forced = true;
                continue;
            }
            if tokio::time::timeout(remaining, h).await.is_err() {
                // exceeded phase2 budget
                any_forced = true;
            }
        }

        // Forced if any task exceeded phase2 OR total elapsed exceeded the
        // hard deadline.
        let hard_left = hard_deadline.saturating_sub(start.elapsed());
        let mut outcome = DrainOutcome::Clean;
        if any_forced || hard_left.is_zero() {
            metrics::counter!("gw_shutdown_forced_total").increment(1);
            outcome = DrainOutcome::Forced;
        }

        self.phase
            .store(ShutdownPhase::Stopped as u8, Ordering::Release);
        metrics::gauge!("gw_shutdown_phase").set(3.0);
        outcome
    }

    /// Emergency: skip phases, abort everything, set Stopped immediately.
    pub fn emergency_abort(&mut self) {
        self.token.cancel();
        for h in std::mem::take(&mut self.handles) {
            h.abort();
        }
        self.phase
            .store(ShutdownPhase::Stopped as u8, Ordering::Release);
        metrics::counter!("gw_shutdown_forced_total").increment(1);
    }
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}
