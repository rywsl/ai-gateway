use gw_store::identity_persist::IdentityPersistQueue;
use std::sync::Arc;

#[tokio::test]
async fn dedup_within_pending_window() {
    let writer = Arc::new(noop_writer::Counter::default());
    let q = IdentityPersistQueue::new_with_writer(writer.clone(), 1024);

    let snap = sample_snapshot();
    q.schedule("vk-A".into(), snap.clone());
    q.schedule("vk-A".into(), snap.clone());
    q.schedule("vk-B".into(), snap.clone());
    q.flush_for_test().await;

    assert_eq!(writer.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn drops_when_pending_at_cap() {
    let writer = Arc::new(noop_writer::Counter::default());
    let q = IdentityPersistQueue::new_with_writer(writer.clone(), /*max_pending*/ 1);
    q.schedule("vk-A".into(), sample_snapshot());
    let dropped = q.schedule("vk-B".into(), sample_snapshot());
    assert!(dropped, "second schedule should be dropped (pending at cap)");
}

mod noop_writer {
    use async_trait::async_trait;
    use std::sync::atomic::AtomicUsize;
    #[derive(Default)]
    pub struct Counter { pub calls: AtomicUsize }
    #[async_trait]
    impl gw_store::identity_persist::SnapshotWriter for Counter {
        async fn write_if_null(
            &self, _key_id: &str,
            _snap: &gw_core::impersonate::IdentitySnapshot,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }
}

fn sample_snapshot() -> gw_core::impersonate::IdentitySnapshot {
    gw_core::impersonate::IdentitySnapshot {
        profile_kind: gw_core::impersonate::ImpersonateProfile::ClaudeCodeChrome131,
        variable: gw_core::impersonate::claude_code::derive("vk-sample"),
    }
}
