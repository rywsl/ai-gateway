pub mod types;
pub use types::*;

pub mod adapter_hook; // T4-C
pub mod backend; // T3-C — production EntityStore-backed ResolveBackend
pub mod filter;
pub mod mapper; // T4-A
pub mod pricing;
pub mod resolver; // T4-B

pub use backend::EntityStoreResolveBackend;
pub use filter::{candidate_matches, parse_capabilities};
pub use resolver::{CapabilityResolver, ResolveBackend, ResolveRequest};
