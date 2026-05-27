//! gw-health: multi-dim KeyHealth state machine (`(KeyId, ApiFormat) →
//! FormatHealth`). Lands the K-A multi-dim per-(key, format) circuit +
//! cooldown design per integration spec §2 表 A A2 + K-A ADR-KA-1
//! (replaces the retired single-dim circuit that previously lived in
//! gw-routing).
//!
//! No persistence in v1 (per K-A ADR-KA-11): each gateway replica
//! maintains its own snapshot.

pub mod cooldown;
pub mod format_health;
pub mod quota_probe;
pub mod sliding_window;
pub mod store;

pub use cooldown::{CircuitState, CooldownDirective, CooldownReason};
pub use format_health::FormatHealth;
pub use quota_probe::{ProbeResult, ProbeTransport, QuotaProbeCfg, QuotaProbeRunner};
pub use sliding_window::SlidingWindow;
pub use store::{HealthSnapshot, KeyHealthStore, OutcomeKind};
