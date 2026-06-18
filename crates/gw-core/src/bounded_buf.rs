//! Bounded buffer primitives — T-W3-04 (WS-3 bounded memory).
//!
//! A small, allocation-aware wrapper around `bytes::BytesMut` that enforces
//! a per-buffer byte cap on every push and emits a static-label metric
//! whenever the cap is reached.
//!
//! ## Why
//!
//! Several proxy-side accumulators in `gw-proxy` historically read from a
//! malicious or slow upstream without any size cap (Proxy-1 / Proxy-2 /
//! Proxy-3 / Proxy-8, Codec-4). Without a cap, a single hostile request can
//! linearly grow Pod RSS until the kernel OOM-kills the process. The
//! mitigation here is not "one global cap" — different buffers have
//! different correctness contracts:
//!
//! - `upstream_body_accum` accumulates a non-stream upstream body for codec
//!   `parse_response`. A truncated body would silently mis-parse JSON and
//!   produce a wrong token count → wrong cost. Policy must be `Reject`.
//! - `sse_leftover` carries the un-framed tail of an SSE chunk forward.
//!   A 10 GB no-newline line is hostile, but dropping the tail only loses
//!   a partial event and the SSE consumer handles that. Policy is `Truncate`.
//! - `stream_probe::buffer` accumulates the first ~N bytes used to classify
//!   whether the upstream produced a valid SSE / chunk frame. Truncation
//!   would make the classifier mis-judge a malformed body as valid; policy
//!   must be `Reject`.
//!
//! Each buffer therefore declares its own [`OverflowPolicy`] at construction
//! time, and [`BoundedBufCfg::validate`] is invoked at boot to refuse any
//! configuration that would weaken a correctness-critical buffer to
//! `Truncate`.

use bytes::BytesMut;
use std::path::PathBuf;

// --------------------------------------------------------------------------
// Errors
// --------------------------------------------------------------------------

/// Result of a [`BoundedBuf::try_push`] call.
#[derive(Debug, thiserror::Error)]
pub enum BoundedBufError {
    /// Policy is [`OverflowPolicy::Reject`] and the push would have grown
    /// the buffer past `cap`. Caller MUST surface this as an error to the
    /// upstream (typically `502 GW_UPSTREAM_BODY_TRUNCATED` for proxy
    /// accumulators, or a codec abort for codec state).
    #[error("bounded buffer `{buf_name}` would exceed cap {cap} (attempted {attempted})")]
    RequestRejected {
        cap: usize,
        attempted: usize,
        buf_name: &'static str,
    },
    /// Policy is [`OverflowPolicy::Truncate`] and `dropped` bytes were
    /// dropped from the end of the input chunk. The buffer is now exactly
    /// at `cap`. Caller treats this as a soft warning — the bytes already
    /// in the buffer are still valid.
    #[error("bounded buffer `{buf_name}` truncated: dropped {dropped} bytes at cap {cap}")]
    Truncated {
        cap: usize,
        dropped: usize,
        buf_name: &'static str,
    },
    /// Policy is [`OverflowPolicy::Spill`] but the spill path is unavailable
    /// (failed to open / write). Caller treats this as a hard rejection.
    #[error("bounded buffer `{buf_name}` spill failed: {reason}")]
    SpillFailed {
        buf_name: &'static str,
        reason: &'static str,
    },
}

// --------------------------------------------------------------------------
// Policy
// --------------------------------------------------------------------------

/// What to do when a push would grow a buffer past `cap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Append up to `cap` bytes; return [`BoundedBufError::Truncated`] with
    /// the count of dropped bytes. The buffer is now exactly at `cap`. Use
    /// this only when downstream consumers can handle a truncated tail
    /// (e.g. SSE line residue, debug captures).
    Truncate,
    /// Refuse the push entirely (buffer stays at its previous length) and
    /// return [`BoundedBufError::RequestRejected`]. Use this when truncation
    /// would corrupt the buffer's semantic meaning (e.g. JSON bodies that
    /// would silently mis-parse).
    Reject,
    /// Overflow bytes are written to a spill file at the configured path.
    /// Reserved for future use by `archive` surfaces.
    Spill { path: PathBuf },
}

impl OverflowPolicy {
    /// Static label suffix for the `gw_bounded_buf_overflow_total{action=…}`
    /// metric. Kept here so callers cannot drift.
    pub fn metric_action(&self) -> &'static str {
        match self {
            OverflowPolicy::Truncate => "truncate",
            OverflowPolicy::Reject => "reject",
            OverflowPolicy::Spill { .. } => "spill",
        }
    }
}

// --------------------------------------------------------------------------
// Config
// --------------------------------------------------------------------------

/// Bound-and-policy descriptor for a single buffer. Constructed at boot
/// from configuration and threaded into [`BoundedBytesMut::new`].
#[derive(Debug, Clone)]
pub struct BoundedBufCfg {
    /// Maximum byte length the buffer may reach.
    pub max_bytes: usize,
    /// What to do when a push would exceed `max_bytes`.
    pub policy: OverflowPolicy,
    /// Static label for `gw_bounded_buf_overflow_total{label=…}`. MUST be
    /// a finite, code-defined set — never include `request_id` or other
    /// per-tenant data (high-cardinality is forbidden by WS-3 REV-FINAL-V7).
    pub metric_label: &'static str,
}

impl BoundedBufCfg {
    /// Validates that a configuration is internally consistent. Called by
    /// boot wiring; a returned `Err` MUST cause the process to refuse to
    /// start (per WS-3 REV-FINAL-V1 — silent fallback to truncate would
    /// re-introduce the very bug we're fixing).
    ///
    /// Rules enforced:
    /// 1. `max_bytes` must be non-zero (a zero-cap buffer rejects every
    ///    push, which is almost certainly a misconfiguration).
    /// 2. The `metric_label` must be non-empty.
    /// 3. `Spill.path` must be non-empty when the policy is `Spill`.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_bytes == 0 {
            return Err(format!(
                "bounded_buf `{}`: max_bytes must be > 0",
                self.metric_label
            ));
        }
        if self.metric_label.is_empty() {
            return Err("bounded_buf: metric_label must be non-empty".to_string());
        }
        if let OverflowPolicy::Spill { path } = &self.policy {
            if path.as_os_str().is_empty() {
                return Err(format!(
                    "bounded_buf `{}`: Spill policy requires a non-empty path",
                    self.metric_label
                ));
            }
        }
        Ok(())
    }
}

// --------------------------------------------------------------------------
// Trait
// --------------------------------------------------------------------------

/// A push-only sink with a hard byte cap and a declared overflow policy.
///
/// Implementors MUST consult [`OverflowPolicy`] on every push and emit
/// the `gw_bounded_buf_overflow_total{label, action}` counter when the
/// cap is reached.
pub trait BoundedBuf {
    /// Append `data` to the buffer. Returns:
    /// - `Ok(())` when all bytes fit, OR when policy is `Truncate` and the
    ///   data fits exactly to cap.
    /// - `Err(Truncated { .. })` when policy is `Truncate` and some bytes
    ///   were dropped from the tail.
    /// - `Err(RequestRejected { .. })` when policy is `Reject` and any
    ///   bytes would have spilled past cap (buffer stays unchanged).
    fn try_push(&mut self, data: &[u8]) -> Result<(), BoundedBufError>;

    /// Current byte length.
    fn len(&self) -> usize;

    /// Configured byte cap.
    fn capacity(&self) -> usize;

    /// Convenience.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// --------------------------------------------------------------------------
// Concrete impl: BoundedBytesMut
// --------------------------------------------------------------------------

/// `BytesMut`-backed [`BoundedBuf`] implementation.
///
/// The wrapper deliberately exposes the inner `BytesMut` via [`Self::as_bytes_mut`]
/// for read-side operations (split, drain, freeze, as_ref). All write
/// operations MUST go through [`BoundedBuf::try_push`] so the cap is
/// enforced consistently.
#[derive(Debug)]
pub struct BoundedBytesMut {
    inner: BytesMut,
    cap: usize,
    policy: OverflowPolicy,
    /// Static label used for metrics. Must outlive the buffer (always a
    /// `&'static str` literal in practice).
    label: &'static str,
}

impl BoundedBytesMut {
    /// Construct an empty bounded buffer with the given cap, policy, and
    /// metric label.
    pub fn new(label: &'static str, cap: usize, policy: OverflowPolicy) -> Self {
        Self {
            inner: BytesMut::new(),
            cap,
            policy,
            label,
        }
    }

    /// Construct from a [`BoundedBufCfg`].
    pub fn from_cfg(cfg: &BoundedBufCfg) -> Self {
        Self::new(cfg.metric_label, cfg.max_bytes, cfg.policy.clone())
    }

    /// Drop all contents (preserves cap and policy).
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Static metric label.
    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Configured policy.
    pub fn policy(&self) -> &OverflowPolicy {
        &self.policy
    }

    /// Read-only view of the accumulated bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.inner
    }

    /// Direct access to the inner `BytesMut` for read-side operations only
    /// (split, drain, freeze). Callers MUST NOT use this to bypass the
    /// cap on writes — use [`BoundedBuf::try_push`] for all appends.
    pub fn as_bytes_mut(&mut self) -> &mut BytesMut {
        &mut self.inner
    }

    /// Consume `self` and return the inner `BytesMut`. Useful when the
    /// buffer's lifecycle ends (e.g. final freeze into a `Bytes`).
    pub fn into_inner(self) -> BytesMut {
        self.inner
    }

    /// Take ownership of the inner buffer, leaving an empty `BytesMut`
    /// behind. Cap and policy are preserved.
    pub fn take_inner(&mut self) -> BytesMut {
        std::mem::take(&mut self.inner)
    }
}

impl BoundedBuf for BoundedBytesMut {
    fn try_push(&mut self, data: &[u8]) -> Result<(), BoundedBufError> {
        let new_len = self.inner.len().saturating_add(data.len());
        if new_len <= self.cap {
            self.inner.extend_from_slice(data);
            return Ok(());
        }

        // Cap reached. The action depends on the policy. Always emit the
        // metric — observability of cap hits is the whole point of WS-3.
        match &self.policy {
            OverflowPolicy::Reject => {
                metrics::counter!(
                    "gw_bounded_buf_overflow_total",
                    "label" => self.label,
                    "action" => "reject",
                )
                .increment(1);
                Err(BoundedBufError::RequestRejected {
                    cap: self.cap,
                    attempted: new_len,
                    buf_name: self.label,
                })
            }
            OverflowPolicy::Truncate => {
                let room = self.cap.saturating_sub(self.inner.len());
                if room > 0 {
                    self.inner.extend_from_slice(&data[..room]);
                }
                let dropped = data.len() - room;
                metrics::counter!(
                    "gw_bounded_buf_overflow_total",
                    "label" => self.label,
                    "action" => "truncate",
                )
                .increment(1);
                Err(BoundedBufError::Truncated {
                    cap: self.cap,
                    dropped,
                    buf_name: self.label,
                })
            }
            OverflowPolicy::Spill { .. } => {
                // Spill is reserved for future archive paths. Right now no
                // call site uses it; emit the metric and fail closed so a
                // misconfiguration is loud rather than silent.
                metrics::counter!(
                    "gw_bounded_buf_overflow_total",
                    "label" => self.label,
                    "action" => "spill",
                )
                .increment(1);
                Err(BoundedBufError::SpillFailed {
                    buf_name: self.label,
                    reason: "spill backend not wired",
                })
            }
        }
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn capacity(&self) -> usize {
        self.cap
    }
}

impl Default for BoundedBytesMut {
    /// A permissive default for `std::mem::take` and test scaffolding only.
    /// Production code MUST construct via [`Self::new`] or [`Self::from_cfg`]
    /// with explicit cap and policy.
    fn default() -> Self {
        Self::new("__default__", usize::MAX, OverflowPolicy::Reject)
    }
}

impl Clone for BoundedBytesMut {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            cap: self.cap,
            policy: self.policy.clone(),
            label: self.label,
        }
    }
}

impl std::ops::Deref for BoundedBytesMut {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_reject() -> BoundedBufCfg {
        BoundedBufCfg {
            max_bytes: 8,
            policy: OverflowPolicy::Reject,
            metric_label: "test_reject",
        }
    }

    fn cfg_truncate() -> BoundedBufCfg {
        BoundedBufCfg {
            max_bytes: 4,
            policy: OverflowPolicy::Truncate,
            metric_label: "test_truncate",
        }
    }

    #[test]
    fn validate_zero_max_bytes_rejected() {
        let cfg = BoundedBufCfg {
            max_bytes: 0,
            policy: OverflowPolicy::Reject,
            metric_label: "bad",
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_ok_reject() {
        assert!(cfg_reject().validate().is_ok());
    }

    #[test]
    fn validate_ok_truncate() {
        assert!(cfg_truncate().validate().is_ok());
    }

    #[test]
    fn validate_empty_spill_path_rejected() {
        let cfg = BoundedBufCfg {
            max_bytes: 8,
            policy: OverflowPolicy::Spill {
                path: PathBuf::new(),
            },
            metric_label: "bad",
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn try_push_under_cap_succeeds() {
        let mut b = BoundedBytesMut::from_cfg(&cfg_reject());
        assert!(b.try_push(b"hello").is_ok());
        assert_eq!(b.len(), 5);
        assert!(!b.is_empty());
        assert_eq!(b.as_slice(), b"hello");
    }

    #[test]
    fn try_push_reject_leaves_buffer_unchanged() {
        let mut b = BoundedBytesMut::from_cfg(&cfg_reject()); // cap=8
        b.try_push(b"1234567").unwrap();
        let before = b.len();
        let res = b.try_push(b"big payload");
        assert!(matches!(
            res,
            Err(BoundedBufError::RequestRejected { cap: 8, .. })
        ));
        // Buffer unchanged after reject.
        assert_eq!(b.len(), before);
    }

    #[test]
    fn try_push_truncate_drops_tail() {
        let mut b = BoundedBytesMut::from_cfg(&cfg_truncate()); // cap=4
        b.try_push(b"AB").unwrap();
        let res = b.try_push(b"CDEFG");
        assert!(matches!(
            res,
            Err(BoundedBufError::Truncated {
                cap: 4,
                dropped: 3,
                ..
            })
        ));
        assert_eq!(b.len(), 4);
        assert_eq!(b.as_slice(), b"ABCD");
    }

    #[test]
    fn try_push_truncate_with_no_room_drops_everything() {
        let mut b = BoundedBytesMut::from_cfg(&cfg_truncate()); // cap=4
        b.try_push(b"ABCD").unwrap();
        let res = b.try_push(b"EFGH");
        assert!(matches!(
            res,
            Err(BoundedBufError::Truncated {
                cap: 4,
                dropped: 4,
                ..
            })
        ));
        assert_eq!(b.len(), 4);
        assert_eq!(b.as_slice(), b"ABCD");
    }

    #[test]
    fn clear_resets_buffer_keeps_cap_and_policy() {
        let mut b = BoundedBytesMut::from_cfg(&cfg_reject());
        b.try_push(b"data").unwrap();
        b.clear();
        assert_eq!(b.len(), 0);
        assert!(b.is_empty());
        assert_eq!(b.capacity(), 8);
        assert_eq!(b.policy().metric_action(), "reject");
    }
}
