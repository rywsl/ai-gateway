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
    /// Conservative defaults sized for long-running LLM completions.
    /// Streaming responses extend `total` to `stream_total` (30 minutes by
    /// default) when [`apply_to_peer`] is called with `is_stream = true`.
    pub fn default_for_nonstream() -> Self {
        Self {
            connect: Duration::from_secs(30),
            read: Duration::from_secs(180),
            total: Duration::from_secs(900),
            stream_total: Duration::from_secs(1800),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeouts_match_long_running_llm_policy() {
        let t = Timeouts::default_for_nonstream();

        assert_eq!(t.connect, Duration::from_secs(30));
        assert_eq!(t.read, Duration::from_secs(180));
        assert_eq!(t.total, Duration::from_secs(900));
        assert_eq!(t.stream_total, Duration::from_secs(1800));
    }
}
