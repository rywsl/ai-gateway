//! sync ↔ stream upstream policy decision.
//!
//! This is interface-only in v1: the actual `AccumulateUpstreamStream` and
//! `SimulateStreamFromSync` paths require codec S4 (`parse_response`) which
//! lands separately. T6-D wires the accumulator; this task only fixes the
//! decision contract.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamStreamPolicy {
    /// Client wants sync, upstream supports sync → forward as-is.
    PassThroughSync,
    /// Client wants sync, upstream is stream-only → gateway accumulates SSE
    /// chunks into a single sync response. Body assembly lives in T6-D.
    AccumulateUpstreamStream,
    /// Client wants stream, upstream supports stream → forward chunks.
    PassThroughStream,
    /// Client wants stream, upstream is sync-only → gateway emits a synthetic
    /// SSE wrapping the sync result.
    SimulateStreamFromSync,
}

#[derive(Debug, Clone, Copy)]
pub struct UpstreamStreamCapability {
    pub supports_sync: bool,
    pub supports_stream: bool,
}

/// Decide which bridging strategy to use.
///
/// Truth table:
///
/// | client wants stream | sync | stream | result |
/// |---|---|---|---|
/// | false | true  | _     | PassThroughSync |
/// | false | false | true  | AccumulateUpstreamStream |
/// | true  | _     | true  | PassThroughStream |
/// | true  | true  | false | SimulateStreamFromSync |
/// | _     | false | false | PassThroughSync (caller error) |
pub fn decide(
    client_wants_stream: bool,
    caps: UpstreamStreamCapability,
) -> UpstreamStreamPolicy {
    match (client_wants_stream, caps.supports_sync, caps.supports_stream) {
        (false, true, _) => UpstreamStreamPolicy::PassThroughSync,
        (false, false, true) => UpstreamStreamPolicy::AccumulateUpstreamStream,
        (true, _, true) => UpstreamStreamPolicy::PassThroughStream,
        (true, true, false) => UpstreamStreamPolicy::SimulateStreamFromSync,
        // Misconfigured: neither sync nor stream advertised. Surface as sync
        // so the upper layer reports a clean failure when it tries to dispatch.
        (_, false, false) => UpstreamStreamPolicy::PassThroughSync,
    }
}
