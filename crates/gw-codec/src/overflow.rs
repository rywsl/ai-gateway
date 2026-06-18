//! Codec overflow guards — T-W3-08 (WS-3 bounded memory).
//!
//! Streaming codecs accumulate per-call state on a per-stream `StreamState`
//! struct: a `Vec` / `HashMap` of `*FunctionCallBuilder`, each of which
//! grows its `arguments` / `args_json` `String` as tool-call argument
//! fragments stream in. Without a cap, a malicious upstream can:
//!
//! 1. Stream 100k unique function calls in one chunk → unbounded `Vec`
//!    growth on `pending_function_calls`. Each builder also allocates.
//! 2. Stream one function call whose `arguments` delta repeats GB of
//!    bytes → unbounded `String` growth on that builder's args buffer.
//!
//! Either path silently exhausts Pod RSS while the proxy is still happy
//! to return 200 OK to the client.
//!
//! This module provides:
//!
//! - [`CodecOverflowError`], the single error type a cap-aware state
//!   helper returns.
//! - Per-kind metric label constants used with
//!   `gw_codec_overflow_total{kind=…}`.
//! - Default cap constants conservative enough that legitimate traffic
//!   never trips them (128 calls / 4 MiB args), but small enough that
//!   the hostile case is bounded.
//!
//! The `gw-codec` state structs in `gemini::state` and
//! `openai::responses::state` consult these caps via `try_push_function_call`
//! and `try_extend_arguments`. When the cap trips, the codec drops the
//! offending payload, increments the counter, and continues — the
//! downstream emit lifecycle (`ctx.usage.aborted` + SSE error frame) is
//! orchestrated by `gw-proxy` if it is wired; this crate stays
//! ctx-free per the gw-codec / gw-proxy layering rule.

use thiserror::Error;

/// Default upper bound on the number of concurrent function-call builders
/// per stream (Vec / HashMap length). 128 is two orders of magnitude
/// larger than any legitimate Anthropic / OpenAI / Gemini response we have
/// observed (real responses cap at single digits).
pub const DEFAULT_MAX_PENDING_FUNCTION_CALLS: usize = 128;

/// Default upper bound on a single function-call builder's accumulated
/// arguments JSON string length. 4 MiB is generous (legitimate JSON tool
/// arguments are typically < 64 KiB) but small enough that 100 simultaneous
/// builders at full cap stay under 512 MiB of process RSS.
pub const DEFAULT_MAX_ACCUMULATED_BYTES: usize = 4 * 1024 * 1024;

/// Reasons a codec state push refused to accept more data. Surfaced via
/// `gw_codec_overflow_total{kind=…}` and (when the caller is willing to
/// abort the stream) returned to the proxy so it can mark the request
/// `aborted` and emit a final SSE error frame.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodecOverflowError {
    /// `pending_function_calls.len()` already at the configured cap; the
    /// new call was dropped.
    #[error("codec: too many concurrent function calls (cap {cap})")]
    TooManyFunctionCalls { cap: usize },

    /// A single function-call builder's `arguments` string would exceed
    /// the configured cap; the offending delta was dropped.
    #[error("codec: function-call arguments buffer would exceed cap {cap}")]
    ArgumentsBufferCap { cap: usize },

    /// Reserved for future use: total accumulated bytes across all
    /// per-stream builders would exceed the configured aggregate cap.
    #[error("codec: total accumulated bytes would exceed cap {cap}")]
    AccumulatedBytesCap { cap: usize },
}

impl CodecOverflowError {
    /// Static metric label suffix for `gw_codec_overflow_total{kind=…}`.
    pub fn metric_kind(&self) -> &'static str {
        match self {
            CodecOverflowError::TooManyFunctionCalls { .. } => "too_many_function_calls",
            CodecOverflowError::ArgumentsBufferCap { .. } => "args_buffer_cap",
            CodecOverflowError::AccumulatedBytesCap { .. } => "accumulated_bytes",
        }
    }

    /// Increment `gw_codec_overflow_total{kind=…}`. The metric is on a
    /// finite, code-defined set of `kind` values (no per-tenant /
    /// per-request_id leakage; T-W3-21 / REV-FINAL-V7).
    pub fn record_metric(&self) {
        metrics::counter!(
            "gw_codec_overflow_total",
            "kind" => self.metric_kind(),
        )
        .increment(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_kinds_are_static() {
        assert_eq!(
            CodecOverflowError::TooManyFunctionCalls { cap: 128 }.metric_kind(),
            "too_many_function_calls",
        );
        assert_eq!(
            CodecOverflowError::ArgumentsBufferCap { cap: 4 << 20 }.metric_kind(),
            "args_buffer_cap",
        );
        assert_eq!(
            CodecOverflowError::AccumulatedBytesCap { cap: 16 << 20 }.metric_kind(),
            "accumulated_bytes",
        );
    }
}
