//! License gate. WS-15 T15.4 Rust-side wiring.
//!
//! Reads the runtime flag `[flags].license_enforce` (disabled / warn / enforce)
//! and gates license-violation handling on it:
//! - `disabled`: no-op (never blocks, never meters).
//! - `warn`:     emit `gateway_license_violation_total{mode="warn",reason}` and
//!   pass the request through. Response bytes are identical to
//!   `disabled` — warn mode MUST NOT alter the response.
//! - `enforce`:  emit the metric and return `Err(LicenseError)` so the caller
//!   rejects the request (HTTP 402 `GW_LICENSE_REQUIRED`).
//!
//! Default is `warn` so a misconfigured deployment never locks itself out
//! (REV-FINAL-LICENSE-COMM / v2 REV-004e): a missing/typo'd flag resolves to
//! warn, not enforce.
//!
//! This module owns only the flag-gate framework + warn-mode metric + the
//! enforce-mode rejection decision. The real `LicenseStatus` source (license
//! service, feature flags, node-count probe) lands with WS-5 / WS-16; until
//! then `request_filter` calls `enforce` with `LicenseStatus::Valid` so the
//! skeleton is wired but inert. The 402 rejection path exists in the `Enforce`
//! arm below and activates the moment a real status feeds a violation.

use std::sync::atomic::{AtomicU8, Ordering};

/// Licence enforcement mode, mirrors `gw_gateway::config::LicenseEnforceMode`.
/// Kept as a self-contained enum here so `gw-auth` does not depend on
/// `gw-gateway` (which would be a layering cycle).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LicenseEnforceMode {
    Disabled,
    Warn,
    Enforce,
}

impl LicenseEnforceMode {
    const fn as_u8(self) -> u8 {
        match self {
            LicenseEnforceMode::Disabled => 0,
            LicenseEnforceMode::Warn => 1,
            LicenseEnforceMode::Enforce => 2,
        }
    }

    const fn from_u8(v: u8) -> Self {
        match v {
            2 => LicenseEnforceMode::Enforce,
            0 => LicenseEnforceMode::Disabled,
            // Any unexpected value collapses to Warn (the safe default —
            // never lock out on a corrupted atomic).
            _ => LicenseEnforceMode::Warn,
        }
    }
}

/// Licence violation outcome. `enforce` mode returns the matching variant so
/// the caller can map it to HTTP 402 `GW_LICENSE_REQUIRED`.
#[derive(Debug, thiserror::Error)]
pub enum LicenseError {
    #[error("license expired")]
    Expired,
    #[error("license feature missing: {0}")]
    FeatureMissing(String),
    #[error("license node count exceeded")]
    NodeCountExceeded,
}

/// Snapshot of the licence state for a single request. Built by the caller
/// (the request filter) from whatever licence source is live; `Valid` short
/// the gate to `Ok(())`.
#[derive(Debug, Clone)]
pub enum LicenseStatus {
    Valid,
    Expired,
    FeatureMissing(String),
    NodeCountExceeded,
}

impl LicenseStatus {
    /// Map a status to the shared `reason` label string used by both the Go
    /// and Rust metric emitters (`"expired" | "feature_missing" |
    /// "node_count_exceeded"`). `Valid` returns `None` (no violation).
    fn violation_reason(&self) -> Option<&'static str> {
        match self {
            LicenseStatus::Valid => None,
            LicenseStatus::Expired => Some("expired"),
            LicenseStatus::FeatureMissing(_) => Some("feature_missing"),
            LicenseStatus::NodeCountExceeded => Some("node_count_exceeded"),
        }
    }
}

/// Per-request context handed to [`LicenseGate::enforce`].
pub struct LicenseContext<'a> {
    pub license_status: &'a LicenseStatus,
    pub feature: Option<&'a str>,
}

/// Hot-reloadable licence gate. Mode is held in an `AtomicU8` so a control-
/// plane reload can flip disabled→warn→enforce without a restart.
///
/// `Clone` is cheap (the inner `Arc` is shared) so the gate can be threaded
/// through `Wired` → `LlmGateway` → `request_filter` by reference.
#[derive(Debug)]
pub struct LicenseGate {
    mode: AtomicU8,
}

impl LicenseGate {
    pub fn new(mode: LicenseEnforceMode) -> Self {
        Self {
            mode: AtomicU8::new(mode.as_u8()),
        }
    }

    /// Hot-reload the enforcement mode (control-plane driven). Takes effect on
    /// the next `enforce` call.
    pub fn set_mode(&self, mode: LicenseEnforceMode) {
        self.mode.store(mode.as_u8(), Ordering::Release);
    }

    /// Current mode (observed with acquire ordering so a concurrent
    /// `set_mode` is visible).
    pub fn mode(&self) -> LicenseEnforceMode {
        LicenseEnforceMode::from_u8(self.mode.load(Ordering::Acquire))
    }

    /// Gate a request against the licence status.
    ///
    /// - `disabled` / no violation → `Ok(())`
    /// - `warn` + violation        → metric ticked, `Ok(())` (response unchanged)
    /// - `enforce` + violation     → metric ticked, `Err(LicenseError)`
    pub fn enforce(&self, ctx: &LicenseContext<'_>) -> Result<(), LicenseError> {
        let mode = self.mode();
        let Some(reason) = ctx.license_status.violation_reason() else {
            return Ok(());
        };
        match mode {
            LicenseEnforceMode::Disabled => Ok(()),
            LicenseEnforceMode::Warn => {
                metric_tick("warn", reason);
                Ok(())
            }
            LicenseEnforceMode::Enforce => {
                metric_tick("enforce", reason);
                Err(match ctx.license_status {
                    LicenseStatus::Expired => LicenseError::Expired,
                    LicenseStatus::FeatureMissing(f) => LicenseError::FeatureMissing(f.clone()),
                    LicenseStatus::NodeCountExceeded => LicenseError::NodeCountExceeded,
                    LicenseStatus::Valid => unreachable!("violation_reason returned Some"),
                })
            }
        }
    }
}

fn metric_tick(mode: &'static str, reason: &'static str) {
    metrics::counter!("gateway_license_violation_total", "mode" => mode, "reason" => reason)
        .increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(status: &LicenseStatus) -> LicenseContext<'_> {
        LicenseContext {
            license_status: status,
            feature: None,
        }
    }

    #[test]
    fn valid_status_passes_in_every_mode() {
        let valid = LicenseStatus::Valid;
        for mode in [
            LicenseEnforceMode::Disabled,
            LicenseEnforceMode::Warn,
            LicenseEnforceMode::Enforce,
        ] {
            let gate = LicenseGate::new(mode);
            assert!(
                gate.enforce(&ctx(&valid)).is_ok(),
                "mode {mode:?} should pass"
            );
        }
    }

    #[test]
    fn disabled_allows_violation() {
        let gate = LicenseGate::new(LicenseEnforceMode::Disabled);
        let expired = LicenseStatus::Expired;
        assert!(gate.enforce(&ctx(&expired)).is_ok());
    }

    #[test]
    fn warn_allows_violation_but_does_not_block() {
        let gate = LicenseGate::new(LicenseEnforceMode::Warn);
        let expired = LicenseStatus::Expired;
        assert!(gate.enforce(&ctx(&expired)).is_ok());
        assert_eq!(gate.mode(), LicenseEnforceMode::Warn);
    }

    #[test]
    fn enforce_rejects_expired() {
        let gate = LicenseGate::new(LicenseEnforceMode::Enforce);
        let expired = LicenseStatus::Expired;
        let err = gate
            .enforce(&ctx(&expired))
            .expect_err("enforce should reject");
        assert!(matches!(err, LicenseError::Expired), "got {err:?}");
    }

    #[test]
    fn enforce_rejects_feature_missing() {
        let gate = LicenseGate::new(LicenseEnforceMode::Enforce);
        let status = LicenseStatus::FeatureMissing("anthropic_messages".to_string());
        let err = gate
            .enforce(&ctx(&status))
            .expect_err("enforce should reject");
        match err {
            LicenseError::FeatureMissing(f) => assert_eq!(f, "anthropic_messages"),
            other => panic!("expected FeatureMissing, got {other:?}"),
        }
    }

    #[test]
    fn enforce_rejects_node_count_exceeded() {
        let gate = LicenseGate::new(LicenseEnforceMode::Enforce);
        let status = LicenseStatus::NodeCountExceeded;
        let err = gate
            .enforce(&ctx(&status))
            .expect_err("enforce should reject");
        assert!(
            matches!(err, LicenseError::NodeCountExceeded),
            "got {err:?}"
        );
    }

    #[test]
    fn set_mode_hot_reloads() {
        let gate = LicenseGate::new(LicenseEnforceMode::Warn);
        assert_eq!(gate.mode(), LicenseEnforceMode::Warn);
        let expired = LicenseStatus::Expired;
        assert!(gate.enforce(&ctx(&expired)).is_ok());

        gate.set_mode(LicenseEnforceMode::Enforce);
        assert_eq!(gate.mode(), LicenseEnforceMode::Enforce);
        assert!(gate.enforce(&ctx(&expired)).is_err());
    }

    #[test]
    fn corrupted_atomic_collapses_to_warn() {
        // Simulate a corrupted store (e.g. memory corruption) — from_u8 must
        // never produce Enforce from an unknown value.
        assert_eq!(LicenseEnforceMode::from_u8(255), LicenseEnforceMode::Warn);
        assert_eq!(LicenseEnforceMode::from_u8(99), LicenseEnforceMode::Warn);
    }

    #[test]
    fn mode_round_trips_through_u8() {
        for mode in [
            LicenseEnforceMode::Disabled,
            LicenseEnforceMode::Warn,
            LicenseEnforceMode::Enforce,
        ] {
            assert_eq!(LicenseEnforceMode::from_u8(mode.as_u8()), mode);
        }
    }
}
