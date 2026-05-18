//! Per-request timeout policy.
//!
//! Defaults come from `Timeouts::default_for_nonstream`. The proxy may override
//! any field from `Pricing.{connect,read,total,stream_total}_timeout_ms` (when
//! present) before calling [`apply_to_peer`].

use pingora::upstreams::peer::HttpPeer;
use std::time::Duration;

pub struct Timeouts {
    pub connect: Duration,
    pub read: Duration,
    pub total: Duration,
    pub stream_total: Duration,
}

impl Timeouts {
    /// Conservative defaults sized for OpenAI-style non-stream completions.
    /// Streaming responses extend `total` to `stream_total` (10 minutes by
    /// default) when [`apply_to_peer`] is called with `is_stream = true`.
    pub fn default_for_nonstream() -> Self {
        Self {
            connect: Duration::from_secs(5),
            read: Duration::from_secs(30),
            total: Duration::from_secs(60),
            stream_total: Duration::from_secs(600),
        }
    }

    pub fn apply_to_peer(&self, peer: &mut HttpPeer, is_stream: bool) {
        peer.options.connection_timeout = Some(self.connect);
        peer.options.read_timeout = Some(self.read);
        // ADR: pingora's `total_connection_timeout` caps the *entire* connect
        // round-trip including TLS — for streaming we relax it to the stream
        // budget so a long-running SSE doesn't get killed mid-flight.
        peer.options.total_connection_timeout = Some(if is_stream {
            self.stream_total
        } else {
            self.total
        });
    }
}
