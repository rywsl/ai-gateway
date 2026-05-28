//! gw-core: 纯类型层（无 IO / 无 async）。所有上层 crate 共享的 IR / ErrorIR / Versioned / ID newtype 集中于此。

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
pub mod usage;
pub mod versioned;

pub mod ratelimit {
    /// Owned by RL-Core (2026-05-19-ratelimit-rl-core.md). 此处仅占位，待 RL-Core 落地后整体替换为 use rl_core::*。
    #[non_exhaustive]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum RateLimitKind {
        Rpm,
        Tpm,
        Daily,
        Monthly,
        Concurrent,
        Unknown,
    }
}
