//! Unified error observability sink.
//!
//! All non-trivial error events (short-circuit bail, upstream connect failure,
//! codec parse warn, background worker recover/fatal) call [`observe`].
//! See `docs/architecture.md`.

use std::fmt;

#[derive(Copy, Clone, Debug)]
pub enum ErrorKind {
    ShortCircuit,
    UpstreamConnect,
    UpstreamMidStream,
    UpstreamProtocol,
    CodecParse,
    CodecRender,
    BackgroundWorker,
}

impl ErrorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ShortCircuit => "short_circuit",
            Self::UpstreamConnect => "upstream_connect",
            Self::UpstreamMidStream => "upstream_mid_stream",
            Self::UpstreamProtocol => "upstream_protocol",
            Self::CodecParse => "codec_parse",
            Self::CodecRender => "codec_render",
            Self::BackgroundWorker => "background_worker",
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum Stage {
    Request,
    Upstream,
    Response,
    Logging,
    Background,
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Upstream => "upstream",
            Self::Response => "response",
            Self::Logging => "logging",
            Self::Background => "background",
        }
    }
}

pub struct ErrorObservation<'a> {
    /// Gateway error code (e.g. `"gw_upstream_connect_failed"`); becomes the `code` label.
    pub code: &'a str,
    /// Coarse error category; becomes the `kind` label.
    pub kind: ErrorKind,
    /// Pipeline stage where the error originated; becomes the `stage` label.
    pub stage: Stage,
    /// HTTP status if known (None at upstream-connect time, Some(401)+ on short-circuit).
    pub status: Option<u16>,
    /// Trace ID for correlation with the detailed log surface.
    pub trace_id: Option<&'a str>,
    /// Free-form debug context; logged via tracing, NOT emitted as a metric label.
    pub extra: Option<&'a dyn fmt::Debug>,
}

/// Emit a unified error observation: increments `gw_errors_total{code,kind,stage}` and logs a `tracing::warn!` on target `"gw_errors"`. Increments on every call (no dedup).
pub fn observe(o: ErrorObservation<'_>) {
    metrics::counter!(
        "gw_errors_total",
        "code" => o.code.to_owned(),
        "kind" => o.kind.as_str(),
        "stage" => o.stage.as_str(),
    )
    .increment(1);

    // Cross-crate aggregation target — flat name (not gw_core::observability) so all error sources surface under one tracing scope.
    tracing::warn!(
        target: "gw_errors",
        code = o.code,
        kind = o.kind.as_str(),
        stage = o.stage.as_str(),
        status = ?o.status,
        trace_id = o.trace_id,
        extra = ?o.extra,
        "error observed",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_util::debugging::{DebuggingRecorder, Snapshotter};

    fn install_recorder() -> Snapshotter {
        let r = DebuggingRecorder::new();
        let snap = r.snapshotter();
        let _ = r.install();
        snap
    }

    #[test]
    fn observe_increments_gw_errors_total_with_expected_labels() {
        let snap = install_recorder();
        observe(ErrorObservation {
            code: "test_code",
            kind: ErrorKind::ShortCircuit,
            stage: Stage::Request,
            status: Some(401),
            trace_id: Some("trace-abc"),
            extra: None,
        });
        let metrics = snap.snapshot().into_vec();
        let counter = metrics
            .iter()
            .find(|(key, _, _, _)| key.key().name() == "gw_errors_total")
            .expect("gw_errors_total recorded");
        let labels: Vec<_> = counter
            .0
            .key()
            .labels()
            .map(|l| (l.key().to_string(), l.value().to_string()))
            .collect();
        assert!(labels.contains(&("code".into(), "test_code".into())));
        assert!(labels.contains(&("kind".into(), "short_circuit".into())));
        assert!(labels.contains(&("stage".into(), "request".into())));
    }
}
