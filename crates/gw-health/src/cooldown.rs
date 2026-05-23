//! Cooldown reason enum + directive struct.
//!
//! `CooldownReason` is a `u16` encoding stored in an atomic on `FormatHealth`.
//! 16 variants today; the discriminants are stable and additive
//! (don't renumber existing entries — observability dashboards key off the
//! numeric label).
//!
//! Per K-A ADR-KA-2 / ADR-KA-5.

/// Reason for an explicit cooldown. Numeric discriminants are stable;
/// never renumber an existing variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum CooldownReason {
    None = 0,
    Status401AccountDeactivated = 1,
    Status401Unauthorized = 2,
    Status402PaymentRequired = 3,
    Status403AccountSuspended = 4,
    Status403Forbidden = 5,
    Status400OrganizationDisabled = 6,
    Status429RateLimit = 7,
    Status529Overloaded = 8,
    StatusTransient5xx = 9,
    Status408Timeout = 10,
    UnschedulableRule = 11,
    OAuthRefreshFailed = 12,
    AccountBlocked = 13,
    AdminForce = 14,
    SlidingWindowTrip = 15,
}

impl CooldownReason {
    /// Decode a stored `u16` back to the typed enum, or [`Self::None`] if the
    /// value is unrecognised (forward compatibility — a newer process may
    /// have written a value an older binary doesn't know).
    pub fn from_u16(v: u16) -> Self {
        match v {
            1 => Self::Status401AccountDeactivated,
            2 => Self::Status401Unauthorized,
            3 => Self::Status402PaymentRequired,
            4 => Self::Status403AccountSuspended,
            5 => Self::Status403Forbidden,
            6 => Self::Status400OrganizationDisabled,
            7 => Self::Status429RateLimit,
            8 => Self::Status529Overloaded,
            9 => Self::StatusTransient5xx,
            10 => Self::Status408Timeout,
            11 => Self::UnschedulableRule,
            12 => Self::OAuthRefreshFailed,
            13 => Self::AccountBlocked,
            14 => Self::AdminForce,
            15 => Self::SlidingWindowTrip,
            _ => Self::None,
        }
    }

    pub fn as_u16(self) -> u16 {
        self as u16
    }

    /// Static label for metrics + tracing. Matches the variant name in
    /// snake_case.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Status401AccountDeactivated => "status401_account_deactivated",
            Self::Status401Unauthorized => "status401_unauthorized",
            Self::Status402PaymentRequired => "status402_payment_required",
            Self::Status403AccountSuspended => "status403_account_suspended",
            Self::Status403Forbidden => "status403_forbidden",
            Self::Status400OrganizationDisabled => "status400_organization_disabled",
            Self::Status429RateLimit => "status429_rate_limit",
            Self::Status529Overloaded => "status529_overloaded",
            Self::StatusTransient5xx => "status_transient_5xx",
            Self::Status408Timeout => "status408_timeout",
            Self::UnschedulableRule => "unschedulable_rule",
            Self::OAuthRefreshFailed => "oauth_refresh_failed",
            Self::AccountBlocked => "account_blocked",
            Self::AdminForce => "admin_force",
            Self::SlidingWindowTrip => "sliding_window_trip",
        }
    }
}

/// Directive passed to [`super::store::KeyHealthStore::apply_cooldown`].
#[derive(Clone, Copy, Debug)]
pub struct CooldownDirective {
    pub reason: CooldownReason,
    pub ttl_secs: u32,
    /// If true, clear `consecutive_failures` on apply (e.g. OAuth refresh OK,
    /// admin unblock). If false, accumulate so graded cooldown can back off.
    pub reset_consecutive: bool,
}

/// Circuit state for `FormatHealth`. Encoded as `u8` in an atomic for
/// CAS-driven open → half_open transitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CircuitState {
    Closed = 0,
    Open = 1,
    HalfOpen = 2,
}

impl CircuitState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Open,
            2 => Self::HalfOpen,
            _ => Self::Closed,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_reason_roundtrips_through_u16() {
        for r in [
            CooldownReason::None,
            CooldownReason::Status401AccountDeactivated,
            CooldownReason::SlidingWindowTrip,
            CooldownReason::AdminForce,
        ] {
            assert_eq!(CooldownReason::from_u16(r.as_u16()), r);
        }
    }

    #[test]
    fn unknown_u16_decodes_as_none() {
        assert_eq!(CooldownReason::from_u16(9999), CooldownReason::None);
    }

    #[test]
    fn circuit_state_roundtrips() {
        for c in [
            CircuitState::Closed,
            CircuitState::Open,
            CircuitState::HalfOpen,
        ] {
            assert_eq!(CircuitState::from_u8(c.as_u8()), c);
        }
    }
}
