//! BillingTransaction state machine. Spec §4.1.
//!
//! The 5-state `VendorExposure` enum tracks how far the request has progressed
//! against the upstream vendor. `Cancelling` is intentionally absent — cancel
//! grace handling is internal to `UpstreamCancellationCoordinator` (added in
//! W3), the public state machine exposes only `Streaming → Terminated`.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VendorExposure {
    #[default]
    Idle,
    Sent,
    Streaming,
    Settled,
    Terminated,
}

pub fn can_transition(from: VendorExposure, to: VendorExposure) -> bool {
    use VendorExposure::*;
    matches!(
        (from, to),
        (Idle, Sent)
            | (Sent, Streaming)
            | (Sent, Terminated)
            | (Streaming, Settled)
            | (Streaming, Terminated)
    )
}

pub fn is_terminal(v: VendorExposure) -> bool {
    matches!(v, VendorExposure::Settled | VendorExposure::Terminated)
}
