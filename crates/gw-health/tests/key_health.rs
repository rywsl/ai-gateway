//! KeyHealthStore behavioural tests.
//!
//! Coverage targets the K-A spec invariants:
//! - (KeyId, ApiFormat) isolation: same KeyId, different ApiFormat have
//!   independent state.
//! - is_available priority: cooldown overrides circuit.
//! - Half-open CAS admits exactly one probe.
//! - apply_cooldown monotonic — can't shorten an in-flight window.
//! - record() trips the circuit only after the 20-sample / 50%-rate floor.

use gw_core::ids::KeyId;
use gw_core::protocol::ProtocolKind;
use gw_health::cooldown::{CircuitState, CooldownDirective, CooldownReason};
use gw_health::store::{KeyHealthStore, OutcomeKind};

#[test]
fn fresh_store_is_available_for_unknown_key() {
    let s = KeyHealthStore::default();
    assert!(s.is_available(&KeyId::from("k-fresh"), ProtocolKind::OpenAi));
}

#[test]
fn cooldown_overrides_closed_circuit() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-cooldown");
    assert!(s.is_available(&k, ProtocolKind::OpenAi));

    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::Status401AccountDeactivated,
            ttl_secs: 3600,
            reset_consecutive: false,
        },
    );
    assert!(!s.is_available(&k, ProtocolKind::OpenAi));
}

#[test]
fn cooldown_isolated_across_api_formats() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-multi-fmt");
    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::Status429RateLimit,
            ttl_secs: 60,
            reset_consecutive: false,
        },
    );
    assert!(!s.is_available(&k, ProtocolKind::OpenAi));
    // Anthropic bucket is untouched.
    assert!(s.is_available(&k, ProtocolKind::Anthropic));
}

#[test]
fn record_under_floor_does_not_trip_circuit() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-under-floor");
    // 19 failures < 20-sample floor. Use a status that records a failure but
    // does not trigger the payload-driven cooldown path.
    for _ in 0..19 {
        s.record(
            &k,
            ProtocolKind::OpenAi,
            OutcomeKind::Failure {
                http_status: 400,
                retry_after_secs: None,
            },
        );
    }
    let snap = s.peek(&k, ProtocolKind::OpenAi);
    assert_eq!(snap.circuit, CircuitState::Closed);
    assert!(s.is_available(&k, ProtocolKind::OpenAi));
}

#[test]
fn record_over_floor_trips_circuit_to_open() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-trip");
    // 20 failures, 100% rate ⇒ trip.
    for _ in 0..20 {
        s.record(
            &k,
            ProtocolKind::OpenAi,
            OutcomeKind::Failure {
                http_status: 500,
                retry_after_secs: None,
            },
        );
    }
    let snap = s.peek(&k, ProtocolKind::OpenAi);
    assert_eq!(snap.circuit, CircuitState::Open);
    assert!(!s.is_available(&k, ProtocolKind::OpenAi));
}

#[test]
fn apply_cooldown_is_monotonic() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-monotonic");
    // Long cooldown first.
    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::AdminForce,
            ttl_secs: 3600,
            reset_consecutive: false,
        },
    );
    let snap1 = s.peek(&k, ProtocolKind::OpenAi);
    let deadline1 = snap1.cooldown_until_ms.unwrap();

    // Short cooldown next — must NOT shorten the window.
    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::Status429RateLimit,
            ttl_secs: 10,
            reset_consecutive: false,
        },
    );
    let snap2 = s.peek(&k, ProtocolKind::OpenAi);
    assert_eq!(snap2.cooldown_until_ms, Some(deadline1));
    // And the reason of the still-active longer cooldown must NOT be
    // overwritten by the shorter directive that lost the max race.
    assert_eq!(snap2.cooldown_reason, CooldownReason::AdminForce);
}

#[test]
fn clear_cooldown_restores_availability() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-clear");
    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::OAuthRefreshFailed,
            ttl_secs: 3600,
            reset_consecutive: false,
        },
    );
    assert!(!s.is_available(&k, ProtocolKind::OpenAi));
    s.clear_cooldown(&k, ProtocolKind::OpenAi);
    assert!(s.is_available(&k, ProtocolKind::OpenAi));
}

#[test]
fn consecutive_failures_resets_on_success() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-consecutive");
    for _ in 0..5 {
        s.record(
            &k,
            ProtocolKind::OpenAi,
            OutcomeKind::Failure {
                http_status: 500,
                retry_after_secs: None,
            },
        );
    }
    assert_eq!(s.peek(&k, ProtocolKind::OpenAi).consecutive_failures, 5);
    s.record(&k, ProtocolKind::OpenAi, OutcomeKind::Success);
    assert_eq!(s.peek(&k, ProtocolKind::OpenAi).consecutive_failures, 0);
}

#[test]
fn half_open_admits_exactly_one_probe() {
    use gw_health::format_health::FormatHealth;
    use std::sync::atomic::Ordering;

    let h = FormatHealth::new();
    // Force the circuit Open with an elapsed deadline. This mirrors the
    // post-record state after the sliding window has tripped, then time
    // has passed beyond OPEN_HOLD_MS.
    h.circuit
        .store(CircuitState::Open.as_u8(), Ordering::Release);
    h.open_until_ms.store(0, Ordering::Release);

    // First caller wins the CAS from Open -> HalfOpen.
    assert!(h.is_available(), "first caller should claim the probe slot");
    assert_eq!(
        CircuitState::from_u8(h.circuit.load(Ordering::Acquire)),
        CircuitState::HalfOpen,
    );

    // Second caller (and any concurrent caller) must be rejected: a probe
    // is already in flight.
    assert!(
        !h.is_available(),
        "subsequent callers must NOT be admitted while half_open"
    );
}

#[test]
fn half_open_failure_reopens() {
    // Audit 2026-05-30 §1.4: a failed recovery probe must return the circuit to
    // Open (re-arming the 60s hold), not leave it stuck HalfOpen forever.
    use gw_health::format_health::FormatHealth;
    use std::sync::atomic::Ordering;

    let h = FormatHealth::new();
    // Force Open with an elapsed deadline, then claim the half-open probe.
    h.circuit
        .store(CircuitState::Open.as_u8(), Ordering::Release);
    h.open_until_ms.store(0, Ordering::Release);
    assert!(h.is_available(), "probe slot claimed");
    assert_eq!(
        CircuitState::from_u8(h.circuit.load(Ordering::Acquire)),
        CircuitState::HalfOpen,
    );

    // The probe FAILS.
    h.record(false);

    // Circuit must be back to Open with a future deadline...
    assert_eq!(
        CircuitState::from_u8(h.circuit.load(Ordering::Acquire)),
        CircuitState::Open,
        "a failed half-open probe must reopen the circuit"
    );
    assert!(
        h.open_until_ms.load(Ordering::Acquire) > 0,
        "open_until_ms must be re-armed"
    );
    // ...and the key is unavailable during the new hold (not stuck-unavailable
    // forever, but recoverable once the deadline elapses again).
    assert!(
        !h.is_available(),
        "key unavailable during the re-armed open hold"
    );

    // After the hold elapses, the circuit admits a fresh probe again (proving
    // recovery is possible, unlike the stuck-HalfOpen bug).
    h.open_until_ms.store(0, Ordering::Release);
    assert!(
        h.is_available(),
        "a fresh probe is admitted once the re-armed hold elapses"
    );
}
