//! Tunnel client — connects gateway to remote proxy nodes via aether-hub
//! frame protocol. Wave 2 lands the frame codec; Wave 3 adds pool / resolver
//! / safe_dns / target_filter and the WebSocket transport (T3-G).

pub mod frame;
