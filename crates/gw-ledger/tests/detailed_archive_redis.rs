//! End-to-end: emit one `CapturedSurface` per kind, verify Redis Stream
//! receives 5 entries with the right encoding + decodable payload.
//! Requires Docker / a reachable Redis. Run via `cargo test -- --ignored`.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use bytes::Bytes;
use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, KeysInterface, StreamsInterface};
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};
use gw_core::ids::{RequestId, TraceId};
use gw_ledger::detailed_archive::{
    CapturedSurface, DetailedArchive, DetailedArchiveCfg, Kind, RequestPayload, ResponsePayload,
    SummaryPayload, SurfacePayload,
};
use gw_ledger::encoding::{decode, CompressionCfg, Encoding, TierThresholds};
use std::sync::Arc;
use std::time::Duration;

async fn make_pool() -> RedisPool {
    let url =
        std::env::var("GW_TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let cfg = RedisConfig::from_url(&url).expect("parse redis url");
    let pool = RedisPool::new(
        cfg,
        Some(PerformanceConfig::default()),
        Some(ConnectionConfig::default()),
        Some(ReconnectPolicy::default()),
        1,
    )
    .expect("RedisPool");
    pool.init().await.expect("connect");
    pool
}

fn cfg(stream: &str) -> DetailedArchiveCfg {
    DetailedArchiveCfg {
        stream: stream.to_string(),
        maxlen: 100_000,
        channel_capacity: 1024,
        workers: 2,
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

#[tokio::test]
#[ignore = "needs Redis"]
async fn five_surfaces_round_trip_through_redis() {
    let pool = make_pool().await;
    let stream = format!("usage:detailed:test:{}", std::process::id());
    let archive = DetailedArchive::start(pool.clone(), cfg(&stream), gw_runtime::ShutdownToken::default());

    let req_id = RequestId::from("r_e2e");
    let trace = TraceId::from("t_e2e");

    archive.try_send(CapturedSurface {
        ts_ms: 1,
        request_id: req_id.clone(),
        trace_id: trace.clone(),
        kind: Kind::DownstreamRequest,
        payload: SurfacePayload::Request(
            RequestPayload {
                method: "POST".into(),
                url: "/v1/chat".into(),
                http_version: "HTTP/1.1".into(),
                headers: vec![("authorization".into(), "Bearer x".into())],
                body_b64: String::new(),
            },
            Arc::new(Bytes::from_static(b"{\"model\":\"x\"}")),
        ),
    });

    archive.try_send(CapturedSurface {
        ts_ms: 2,
        request_id: req_id.clone(),
        trace_id: trace.clone(),
        kind: Kind::UpstreamRequest,
        payload: SurfacePayload::Request(
            RequestPayload {
                method: "POST".into(),
                url: "https://api.openai.com/v1/chat".into(),
                http_version: "HTTP/1.1".into(),
                headers: vec![("authorization".into(), "Bearer redacted".into())],
                body_b64: String::new(),
            },
            Arc::new(Bytes::from_static(b"{\"model\":\"x\"}")),
        ),
    });

    let medium = "x".repeat(4096);
    archive.try_send(CapturedSurface {
        ts_ms: 3,
        request_id: req_id.clone(),
        trace_id: trace.clone(),
        kind: Kind::UpstreamResponse,
        payload: SurfacePayload::Response(
            ResponsePayload {
                status: 200,
                headers: vec![],
                body_b64: String::new(),
                archive_truncated: false,
                archive_dropped: 0,
            },
            Arc::new(Bytes::from(medium.into_bytes())),
        ),
    });

    let large = "y".repeat(100_000);
    archive.try_send(CapturedSurface {
        ts_ms: 4,
        request_id: req_id.clone(),
        trace_id: trace.clone(),
        kind: Kind::DownstreamResponse,
        payload: SurfacePayload::Response(
            ResponsePayload {
                status: 200,
                headers: vec![],
                body_b64: String::new(),
                archive_truncated: false,
                archive_dropped: 0,
            },
            Arc::new(Bytes::from(large.into_bytes())),
        ),
    });

    archive.try_send(CapturedSurface {
        ts_ms: 5,
        request_id: req_id.clone(),
        trace_id: trace.clone(),
        kind: Kind::Summary,
        payload: SurfacePayload::Summary(SummaryPayload {
            user_id: None,
            api_key_id: None,
            model: None,
            client_protocol: Some("openai_chat".into()),
            upstream_protocol: Some("openai_chat".into()),
            http_status: 200,
            error_code: None,
            is_stream: false,
            input_tokens: 10,
            output_tokens: 20,
            latency_ms: 100,
            ttft_ms: Some(30),
            tunnel_resolution: Some("direct".into()),
        }),
    });

    tokio::time::sleep(Duration::from_millis(300)).await;

    let client = pool.next();
    let len: u64 = client.xlen(&stream).await.expect("xlen");
    assert_eq!(len, 5, "expected 5 entries, got {len}");

    let entries: Vec<(String, std::collections::HashMap<String, Vec<u8>>)> = client
        .xrange(&stream, "-", "+", None)
        .await
        .expect("xrange");
    assert_eq!(entries.len(), 5);

    let mut seen_kinds = std::collections::HashSet::new();
    for (_id, fields) in &entries {
        let kind = std::str::from_utf8(fields.get("kind").expect("kind"))
            .unwrap()
            .to_string();
        let encoding = std::str::from_utf8(fields.get("encoding").expect("encoding"))
            .unwrap()
            .to_string();
        let payload = fields.get("payload").expect("payload").clone();
        let enc = match encoding.as_str() {
            "raw+json" => Encoding::Raw,
            "lz4+json" => Encoding::Lz4,
            "zstd+json" => Encoding::Zstd,
            other => panic!("unknown encoding {other}"),
        };
        let decoded = decode(enc, &payload).expect("decode");
        let json: serde_json::Value = serde_json::from_slice(&decoded).expect("parse");
        if kind == "downstream_response" {
            assert_eq!(enc, Encoding::Zstd, "large body should land in zstd tier");
            let body_b64 = json.get("body_b64").and_then(|v| v.as_str()).unwrap();
            let decoded_body = B64.decode(body_b64).expect("b64");
            assert_eq!(decoded_body.len(), 100_000);
        }
        seen_kinds.insert(kind);
    }
    let all = [
        "downstream_request",
        "upstream_request",
        "upstream_response",
        "downstream_response",
        "summary",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<std::collections::HashSet<_>>();
    assert_eq!(seen_kinds, all);

    let _: i64 = client.del(&stream).await.unwrap_or(0);
}
