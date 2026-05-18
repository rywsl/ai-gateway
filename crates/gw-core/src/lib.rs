//! gw-core: 纯类型层（无 IO / 无 async）。所有上层 crate 共享的 IR / ErrorIR / Versioned / ID newtype 集中于此。

pub mod error;
pub mod ids;
pub mod ir;
pub mod protocol;
