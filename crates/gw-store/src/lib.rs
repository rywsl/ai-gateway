//! gw-store: Redis + PgSQL cold-load chain, EntityStore (Moka caches), CounterStore,
//! Stream consumer skeleton, in-process deduction state, and AES-256-GCM key encryption.

pub mod counter_dispatch;
pub mod counter_store;
pub mod deduction;
pub mod encrypt;
pub mod entity_store;
pub mod identity_persist;
pub mod loader;
pub mod managed_loop;
pub mod pg_client;
pub mod quota_state;
pub mod redis_client;
pub mod stream_consumer;

pub use counter_store::CounterStore;
pub use deduction::{CircuitReason, DeductionState, UserCircuit, DEDUCT, USER_CIRCUIT};
pub use entity_store::EntityStore;
pub use loader::LoaderCtx;
