use crate::target::StreamTarget;
use gw_core::tunnel_protocol::TunnelProtocol;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> AsyncStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

pub type StreamHandle = Box<dyn AsyncStream>;
pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StreamHandle, TransportError>> + Send + 'a>>;

pub trait Transport: Send + Sync {
    fn open_stream<'a>(&'a self, target: StreamTarget) -> TransportFuture<'a>;

    fn alive(&self) -> bool;

    fn last_heartbeat_age(&self) -> Duration;

    fn active_streams(&self) -> u32 {
        0
    }

    fn tunnel_protocol(&self) -> TunnelProtocol;

    fn close(&self);
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport is not alive")]
    NotAlive,
    #[error("stream cap exceeded")]
    StreamCapExceeded,
    #[error("stream closed")]
    Closed,
    #[error("remote tunnel error: {0}")]
    Remote(String),
    #[error("send failed: {0}")]
    Send(String),
    #[error("unsupported target for transport")]
    UnsupportedTarget,
}
