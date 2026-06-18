//! T-W3-05 / T-W3-06 / T-W3-07 — push-time cap enforcement for the three
//! response-side accumulators that historically had no upper bound:
//!
//! - `ReqCtx::upstream_body_accum`  (non-stream cross-protocol body) — Reject
//! - `ReqCtx::sse_leftover`         (un-framed SSE line residue)     — Truncate
//! - `StreamProbeState::buffer`     (first N bytes of probe)         — Reject
//!
//! ## Why not swap the types to `BoundedBytesMut`?
//!
//! Each of these buffers is read at half a dozen call sites with operations
//! that have no clean equivalent on a wrapper type (`Vec::drain(..idx)`,
//! `BytesMut::split().freeze()`, `std::mem::take` with reassignment).
//! Rewriting every such site at once would touch hundreds of lines across
//! filters/response.rs, gateway.rs, and stream_probe.rs and would create
//! a code-review surface much larger than the bug fix itself.
//!
//! The bug being fixed is **uncapped growth on write**, not "wrong type".
//! We therefore introduce a single shared push helper that every grow-site
//! must go through, leaving the read sites unchanged. The capacity and
//! policy live in this module so they cannot drift from the metric labels.
//! A follow-up may swap the types to `gw_core::bounded_buf::BoundedBytesMut`
//! once the read-side API gap is closed.
//!
//! ## Metric
//!
//! Each overflow trips `gw_bounded_buf_overflow_total{label, action}`, the
//! same counter emitted by `BoundedBytesMut::try_push`. Labels and actions
//! are the static set declared by the helpers below.

use bytes::BytesMut;
use gw_core::bounded_buf::OverflowPolicy;

/// T-W3-05 — upstream_body_accum cap. The accumulator feeds
/// `codec.parse_response`, which silently mis-parses a truncated JSON
/// envelope. Policy MUST be Reject.
pub const UPSTREAM_BODY_ACCUM_CAP_BYTES: usize = 8 * 1024 * 1024; // 8 MiB
pub const UPSTREAM_BODY_ACCUM_LABEL: &str = "upstream_body_accum";

/// T-W3-06 — sse_leftover cap. Holds the un-framed tail of an SSE chunk
/// until the next `\n\n`. Truncating loses the residue (downstream sees
/// a partial event), which is recoverable.
pub const SSE_LEFTOVER_CAP_BYTES: usize = 1024 * 1024; // 1 MiB
pub const SSE_LEFTOVER_LABEL: &str = "sse_leftover";

/// T-W3-07 — stream-probe buffer cap. Used to classify the first chunk
/// as valid SSE / malformed / upstream-error-body. Truncation would let
/// a malformed body slip through; policy MUST be Reject.
pub const STREAM_PROBE_BUFFER_CAP_BYTES: usize = 256 * 1024; // 256 KiB
pub const STREAM_PROBE_BUFFER_LABEL: &str = "stream_probe_buffer";

/// Outcome of a [`push_bounded_bytes`] / [`push_bounded_vec`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// All bytes appended; the buffer is still under cap.
    Appended,
    /// Cap policy is `Truncate`; some tail bytes were dropped. The buffer
    /// is now exactly at cap.
    Truncated { dropped: usize },
    /// Cap policy is `Reject`; no bytes were appended. The buffer is
    /// unchanged at its prior length. Caller MUST surface this as an
    /// error to the upstream (e.g. 502 `GW_UPSTREAM_BODY_TRUNCATED`).
    Rejected { attempted: usize },
}

/// Append `chunk` to `buf`, honouring `cap` and `policy`.
///
/// Records `gw_bounded_buf_overflow_total{label, action}` on any cap hit.
/// `label` and `policy` MUST come from the module-level constants above;
/// inline literals risk metric-label drift.
pub fn push_bounded_bytes(
    buf: &mut BytesMut,
    chunk: &[u8],
    cap: usize,
    label: &'static str,
    policy: OverflowPolicy,
) -> PushOutcome {
    let new_len = buf.len().saturating_add(chunk.len());
    if new_len <= cap {
        buf.extend_from_slice(chunk);
        return PushOutcome::Appended;
    }
    match policy {
        OverflowPolicy::Reject => {
            metrics::counter!(
                "gw_bounded_buf_overflow_total",
                "label" => label,
                "action" => "reject",
            )
            .increment(1);
            PushOutcome::Rejected {
                attempted: new_len,
            }
        }
        OverflowPolicy::Truncate => {
            let room = cap.saturating_sub(buf.len());
            if room > 0 {
                buf.extend_from_slice(&chunk[..room]);
            }
            let dropped = chunk.len() - room;
            metrics::counter!(
                "gw_bounded_buf_overflow_total",
                "label" => label,
                "action" => "truncate",
            )
            .increment(1);
            PushOutcome::Truncated { dropped }
        }
        OverflowPolicy::Spill { .. } => {
            metrics::counter!(
                "gw_bounded_buf_overflow_total",
                "label" => label,
                "action" => "spill",
            )
            .increment(1);
            // Spill backend is not wired for the proxy buffers; degrade
            // to Reject so the misconfiguration is loud.
            PushOutcome::Rejected {
                attempted: new_len,
            }
        }
    }
}

/// `Vec<u8>` analogue of [`push_bounded_bytes`]. Used for `sse_leftover`,
/// which the existing SSE-framing code reads with `drain(..idx).collect()`
/// — a method only `Vec<u8>` provides cleanly.
pub fn push_bounded_vec(
    buf: &mut Vec<u8>,
    chunk: &[u8],
    cap: usize,
    label: &'static str,
    policy: OverflowPolicy,
) -> PushOutcome {
    let new_len = buf.len().saturating_add(chunk.len());
    if new_len <= cap {
        buf.extend_from_slice(chunk);
        return PushOutcome::Appended;
    }
    match policy {
        OverflowPolicy::Reject => {
            metrics::counter!(
                "gw_bounded_buf_overflow_total",
                "label" => label,
                "action" => "reject",
            )
            .increment(1);
            PushOutcome::Rejected {
                attempted: new_len,
            }
        }
        OverflowPolicy::Truncate => {
            let room = cap.saturating_sub(buf.len());
            if room > 0 {
                buf.extend_from_slice(&chunk[..room]);
            }
            let dropped = chunk.len() - room;
            metrics::counter!(
                "gw_bounded_buf_overflow_total",
                "label" => label,
                "action" => "truncate",
            )
            .increment(1);
            PushOutcome::Truncated { dropped }
        }
        OverflowPolicy::Spill { .. } => {
            metrics::counter!(
                "gw_bounded_buf_overflow_total",
                "label" => label,
                "action" => "spill",
            )
            .increment(1);
            PushOutcome::Rejected {
                attempted: new_len,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_when_under_cap() {
        let mut b = BytesMut::new();
        let outcome = push_bounded_bytes(
            &mut b,
            b"hello",
            16,
            "test",
            OverflowPolicy::Reject,
        );
        assert_eq!(outcome, PushOutcome::Appended);
        assert_eq!(&b[..], b"hello");
    }

    #[test]
    fn reject_leaves_buffer_unchanged() {
        let mut b = BytesMut::new();
        b.extend_from_slice(b"existing");
        let before = b.len();
        let outcome = push_bounded_bytes(
            &mut b,
            b"too much extra",
            10,
            "test_reject",
            OverflowPolicy::Reject,
        );
        assert!(matches!(outcome, PushOutcome::Rejected { .. }));
        assert_eq!(b.len(), before);
        assert_eq!(&b[..], b"existing");
    }

    #[test]
    fn truncate_keeps_fill_to_cap() {
        let mut b = BytesMut::new();
        b.extend_from_slice(b"AB");
        let outcome = push_bounded_bytes(
            &mut b,
            b"CDEFG",
            4,
            "test_truncate",
            OverflowPolicy::Truncate,
        );
        assert_eq!(outcome, PushOutcome::Truncated { dropped: 3 });
        assert_eq!(&b[..], b"ABCD");
    }

    #[test]
    fn vec_truncate_keeps_fill_to_cap() {
        let mut v: Vec<u8> = Vec::new();
        v.extend_from_slice(b"X");
        let outcome = push_bounded_vec(
            &mut v,
            b"YZ012",
            3,
            "test_vec_truncate",
            OverflowPolicy::Truncate,
        );
        assert_eq!(outcome, PushOutcome::Truncated { dropped: 3 });
        assert_eq!(v, b"XYZ");
    }

    #[test]
    fn vec_reject_when_already_at_cap() {
        let mut v: Vec<u8> = Vec::new();
        v.extend_from_slice(b"FULL");
        let outcome = push_bounded_vec(
            &mut v,
            b"more",
            4,
            "test_vec_reject",
            OverflowPolicy::Reject,
        );
        assert!(matches!(outcome, PushOutcome::Rejected { .. }));
        assert_eq!(v, b"FULL");
    }
}
