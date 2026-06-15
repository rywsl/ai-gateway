use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
#[non_exhaustive]
pub enum StreamTarget {
    Tcp {
        host: String,
        port: u16,
        tls: bool,
    },
    Http {
        method: http::Method,
        url: url::Url,
        headers: Vec<(String, String)>,
        body_rx: mpsc::Receiver<Bytes>,
        body_tx_response: mpsc::Sender<Bytes>,
        meta_tx_response: oneshot::Sender<crate::aether::meta::ResponseMeta>,
    },
}

impl StreamTarget {
    pub fn tcp_authority(&self) -> Option<String> {
        match self {
            Self::Tcp { host, port, .. } => Some(format!("{host}:{port}")),
            Self::Http { .. } => None,
        }
    }

    pub fn tcp_tls_hint(&self) -> Option<bool> {
        match self {
            Self::Tcp { tls, .. } => Some(*tls),
            Self::Http { .. } => None,
        }
    }
}
