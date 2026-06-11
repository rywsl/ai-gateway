//! Thin compatibility wrapper around `gw_runtime::Supervisor`. Existing call
//! sites continue to call `supervise(bind, pool, shutdown_rx)`; internally
//! this now uses the shared Supervisor primitive.

use crate::handle::EgressBind;
use crate::pool::ClientPool;
use crate::server::{Handler, Listener};
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};
use std::sync::Arc;

pub async fn supervise(
    bind: EgressBind,
    pool: Arc<ClientPool>,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let token = ShutdownToken::new();
    let token_for_listener = token.clone();

    // Bridge the broadcast receiver to the new token.
    let token_for_bridge = token.clone();
    tokio::spawn(async move {
        let _ = shutdown_rx.recv().await;
        token_for_bridge.cancel();
    });

    let bind = Arc::new(bind);
    let handle = Supervisor::new(
        "egress_impersonate",
        RestartPolicy::default(),
        token_for_listener,
    )
    .spawn(move |tok| {
        let bind = (*bind).clone();
        let pool = Arc::clone(&pool);
        async move {
            let handler = Arc::new(Handler::new_with_pool(pool));
            let listener = Listener::bind(bind, handler)
                .await
                .map_err(|e| anyhow::anyhow!("bind failed: {e}"))?;
            let mut handle = listener.spawn();

            tokio::select! {
                res = &mut handle.task => {
                    match res {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(e)) => Err(anyhow::anyhow!("listener error: {e}")),
                        Err(e) => Err(anyhow::anyhow!("listener join error: {e}")),
                    }
                }
                _ = tok.cancelled() => {
                    handle.shutdown().await;
                    Ok(())
                }
            }
        }
    });

    let _ = handle.await;
}
