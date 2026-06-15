use crate::core::Transport;
use crate::registry::ProxyNodeRegistry;
use crate::resolver::ProxyNodeView;
use gw_core::tunnel_protocol::TunnelProtocol;
use http::HeaderMap;
use std::sync::Arc;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};

#[derive(Debug, thiserror::Error)]
pub enum ListenerError {
    #[error("missing or malformed Authorization")]
    Unauthorized,
    #[error("unknown node id")]
    UnknownNode,
    #[error("node has non-aether protocol")]
    WrongProtocol,
    #[error("token mismatch")]
    BadToken,
    #[error("missing required header {0}")]
    MissingHeader(&'static str),
    #[error("malformed header {0}")]
    MalformedHeader(&'static str),
    #[error("inbound capacity exhausted")]
    Saturated,
}

pub struct AuthorizedUpgrade {
    pub node_view: ProxyNodeView,
    pub node_name: String,
    pub max_streams: u32,
}

pub struct InboundListener {
    registry: Arc<dyn ProxyNodeRegistry>,
    attach_tx: mpsc::Sender<(String, Arc<dyn Transport>)>,
    sem: Arc<Semaphore>,
}

impl InboundListener {
    pub fn new(
        registry: Arc<dyn ProxyNodeRegistry>,
        attach_tx: mpsc::Sender<(String, Arc<dyn Transport>)>,
    ) -> Self {
        Self::with_capacity(registry, attach_tx, 256)
    }

    pub fn with_capacity(
        registry: Arc<dyn ProxyNodeRegistry>,
        attach_tx: mpsc::Sender<(String, Arc<dyn Transport>)>,
        capacity: usize,
    ) -> Self {
        Self {
            registry,
            attach_tx,
            sem: Arc::new(Semaphore::new(capacity)),
        }
    }

    pub fn try_acquire_permit(&self) -> Result<OwnedSemaphorePermit, ListenerError> {
        self.sem
            .clone()
            .try_acquire_owned()
            .map_err(|_| ListenerError::Saturated)
    }

    pub fn authorize(&self, headers: &HeaderMap) -> Result<AuthorizedUpgrade, ListenerError> {
        let bearer = headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(ListenerError::Unauthorized)?;
        let node_id = header_str(headers, "x-node-id", "X-Node-Id")?;
        let node_name = header_str(headers, "x-node-name", "X-Node-Name")?;
        let max_streams = header_str(headers, "x-tunnel-max-streams", "X-Tunnel-Max-Streams")?
            .parse::<u32>()
            .map_err(|_| ListenerError::MalformedHeader("X-Tunnel-Max-Streams"))?;

        let node_view = self
            .registry
            .lookup_by_id(node_id)
            .ok_or(ListenerError::UnknownNode)?;
        if node_view.tunnel_protocol != TunnelProtocol::Aether {
            return Err(ListenerError::WrongProtocol);
        }
        if !self.registry.verify_auth_token(node_id, bearer) {
            return Err(ListenerError::BadToken);
        }

        Ok(AuthorizedUpgrade {
            node_view,
            node_name: node_name.to_string(),
            max_streams,
        })
    }

    pub async fn publish(&self, node_id: String, transport: Arc<dyn Transport>) {
        let _ = self.attach_tx.send((node_id, transport)).await;
    }
}

fn header_str<'a>(
    headers: &'a HeaderMap,
    name: &str,
    display: &'static str,
) -> Result<&'a str, ListenerError> {
    headers
        .get(name)
        .ok_or(ListenerError::MissingHeader(display))?
        .to_str()
        .map_err(|_| ListenerError::MalformedHeader(display))
}
