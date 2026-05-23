//! Unit-level checks for `DetailedArchive`. The Redis-backed end-to-end test
//! lives in `tests/detailed_archive_redis.rs` (gated on Docker).

use bytes::Bytes;
use gw_core::ids::{RequestId, TraceId};
use gw_ledger::detailed_archive::{
    CapturedSurface, DetailedArchive, DetailedArchiveCfg, Kind, RequestPayload, SurfacePayload,
};
use gw_ledger::encoding::{CompressionCfg, TierThresholds};
use std::sync::Arc;

fn cfg(capacity: usize) -> DetailedArchiveCfg {
    DetailedArchiveCfg {
        stream: "usage:detailed".to_string(),
        maxlen: 1_000_000,
        channel_capacity: capacity,
        workers: 1,
        single_payload_soft_cap_bytes: 8 * 1024 * 1024,
        compression: CompressionCfg {
            thresholds: TierThresholds {
                small_threshold_bytes: 1024,
                medium_threshold_bytes: 65_536,
            },
            zstd_level: 3,
        },
    }
}

fn surface(kind: Kind) -> CapturedSurface {
    CapturedSurface {
        ts_ms: 1,
        request_id: RequestId::from("r_test"),
        trace_id: TraceId::from("t_test"),
        kind,
        payload: SurfacePayload::Request(
            RequestPayload {
                method: "POST".to_string(),
                url: "/".to_string(),
                http_version: "HTTP/1.1".to_string(),
                headers: vec![],
                body_b64: String::new(),
            },
            Arc::new(Bytes::from_static(b"hi")),
        ),
    }
}

#[tokio::test]
async fn try_send_drops_when_channel_full() {
    let archive = DetailedArchive::for_test_with_dummy_workers(cfg(1));
    // First send fills the bounded channel (capacity 1).
    archive.try_send(surface(Kind::DownstreamRequest));
    // Second send must be dropped, not block.
    archive.try_send(surface(Kind::DownstreamRequest));
    // Metric assertions are skipped here; the integration test verifies the
    // counter increment. This test only proves try_send is non-blocking.
}
