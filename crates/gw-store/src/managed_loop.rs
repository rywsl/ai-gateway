//! ManagedLoop — self-healing background-loop contract.
//!
//! See `docs/superpowers/specs/2026-05-28-systemic-fixes-design.md` §5.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

#[async_trait]
pub trait ManagedLoop: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    async fn ensure(&self) -> Result<(), anyhow::Error> {
        Ok(())
    }

    async fn tick(&self) -> Outcome;
}

pub enum Outcome {
    Continue,
    Sleep(Duration),
    Recover(RecoverReason),
    Fatal(anyhow::Error),
}

pub enum RecoverReason {
    UpstreamResourceGone(&'static str),
    Other(&'static str),
}

pub struct Supervisor;

impl Supervisor {
    pub fn spawn<L: ManagedLoop>(l: Arc<L>) -> JoinHandle<()> {
        let name = l.name();
        tokio::spawn(async move {
            if let Err(e) = l.ensure().await {
                let extra = format!("{name}: {:?}", e);
                gw_core::observability::observe(gw_core::observability::ErrorObservation {
                    code: "ensure_failed",
                    kind: gw_core::observability::ErrorKind::BackgroundWorker,
                    stage: gw_core::observability::Stage::Background,
                    status: None,
                    trace_id: None,
                    extra: Some(&extra),
                });
            }
            loop {
                match l.tick().await {
                    Outcome::Continue => continue,
                    Outcome::Sleep(d) => tokio::time::sleep(d).await,
                    Outcome::Recover(reason) => {
                        let detail: &str = match &reason {
                            RecoverReason::UpstreamResourceGone(s) => s,
                            RecoverReason::Other(s) => s,
                        };
                        let extra = format!("{name}: {detail}");
                        gw_core::observability::observe(gw_core::observability::ErrorObservation {
                            code: "recover",
                            kind: gw_core::observability::ErrorKind::BackgroundWorker,
                            stage: gw_core::observability::Stage::Background,
                            status: None,
                            trace_id: None,
                            extra: Some(&extra),
                        });
                        let _ = l.ensure().await;
                    }
                    Outcome::Fatal(e) => {
                        let extra = format!("{name}: {:?}", e);
                        gw_core::observability::observe(gw_core::observability::ErrorObservation {
                            code: "fatal",
                            kind: gw_core::observability::ErrorKind::BackgroundWorker,
                            stage: gw_core::observability::Stage::Background,
                            status: None,
                            trace_id: None,
                            extra: Some(&extra),
                        });
                        break;
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct MockLoop {
        ensure_calls: AtomicU32,
        tick_calls: AtomicU32,
        recover_after: u32,
    }

    #[async_trait]
    impl ManagedLoop for MockLoop {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn ensure(&self) -> Result<(), anyhow::Error> {
            self.ensure_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn tick(&self) -> Outcome {
            let n = self.tick_calls.fetch_add(1, Ordering::SeqCst);
            if n == self.recover_after {
                Outcome::Recover(RecoverReason::UpstreamResourceGone("test_NOGROUP"))
            } else if n > self.recover_after + 1 {
                Outcome::Fatal(anyhow::anyhow!("test done"))
            } else {
                Outcome::Sleep(Duration::from_millis(1))
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn supervisor_calls_ensure_on_recover() {
        let l = Arc::new(MockLoop {
            ensure_calls: AtomicU32::new(0),
            tick_calls: AtomicU32::new(0),
            recover_after: 2,
        });
        let handle = Supervisor::spawn(l.clone());
        let _ = handle.await;
        // boot ensure + one Recover ensure = 2
        assert_eq!(l.ensure_calls.load(Ordering::SeqCst), 2);
    }
}
