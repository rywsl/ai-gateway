//! gw-core: 纯类型层（无 IO / 无 async）。所有上层 crate 共享的 IR / ErrorIR / Versioned / ID newtype 集中于此。

pub mod control_plane;
pub mod debug;
pub mod entity;
pub mod error;
pub mod ids;
pub mod impersonate;
pub mod ir;
pub mod observability;
pub mod permissions;
pub mod protocol;
pub mod traits;
pub mod tunnel_protocol;
pub mod usage;
pub mod versioned;

pub mod ratelimit;
