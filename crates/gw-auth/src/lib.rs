//! gw-auth — JWT / API-key verification, ADR-023 permission merge,
//! IP block check, HMAC signature, governor-based rate limiting, ADR-024 hard caps.

pub mod apikey;
pub mod hard_cap;
pub mod ip_filter;
pub mod jwt;
pub mod jwt_blacklist;
pub mod merge;
pub mod mgmt;
pub mod ratelimit;
pub mod signature;

/// Authentication context produced by the auth pipeline; consumed by [`merge::merge_permissions`].
#[derive(Clone, Debug)]
pub struct AuthCtx {
    pub user_id: gw_core::ids::UserId,
    pub api_key_id: Option<gw_core::ids::ApiKeyId>,
    pub client_ip: std::net::IpAddr,
}

pub use merge::merge_permissions;
pub use merge::MarkupChainSnapshot;
pub use ratelimit::{RateLimitGuard, RateLimitTicket, StreamSlot};
