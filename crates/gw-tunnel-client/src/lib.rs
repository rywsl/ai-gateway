//! Tunnel client — connects gateway to remote proxy nodes via aether-hub
//! frame protocol. Wave 2 lands the frame codec; Wave 3 adds pool / resolver
//! / safe_dns / target_filter and the WebSocket transport (T3-G).

pub mod frame;
pub mod pool;
pub mod resolver;
pub mod safe_dns;
pub mod target_filter;
pub mod transport;

pub use resolver::{
    resolve_tunnel, ProxyNodeView, TunnelPreference, TunnelResolution, TunnelResolveCtx,
};
