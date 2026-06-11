//! ShutdownToken — full ShutdownCoordinator is in task 1.3; this module
//! provides the cancellation primitive Supervisor depends on.

use tokio_util::sync::CancellationToken;

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

// ShutdownCoordinator stub — populated in task 1.3.
#[doc(hidden)]
pub struct ShutdownCoordinator;
