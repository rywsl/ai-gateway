//! gw-store: Redis + PgSQL cold-load chain, EntityStore (Moka caches), CounterStore,
//! Stream consumer skeleton, in-process deduction state, and AES-256-GCM key encryption.

pub mod counter_store;
pub mod deduction;
pub mod encrypt;
pub mod entity_store;
pub mod loader;
pub mod pg_client;
pub mod redis_client;
pub mod stream_consumer;

pub use counter_store::CounterStore;
pub use deduction::{CircuitReason, DEDUCT, DeductionState, USER_CIRCUIT, UserCircuit};
pub use entity_store::EntityStore;
