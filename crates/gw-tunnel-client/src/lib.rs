//! Tunnel client — connects the gateway to first-party Cnexus proxy nodes.
//! The crate also owns protocol-neutral resolver, safe-DNS, and target-filter
//! helpers used by the gateway tunnel path.

pub mod cnexus;
pub mod core;
pub mod resolver;
pub mod safe_dns;
pub mod target;
pub mod target_filter;

pub use core::{StreamHandle, Transport, TransportError, TransportFuture};
pub use resolver::{
    resolve_tunnel, ProxyNodeView, TunnelPreference, TunnelResolution, TunnelResolveCtx,
};
pub use target::StreamTarget;
