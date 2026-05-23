//! Soak window state + start endpoint (spec §5.1).
//!
//! `start` records XLEN snapshots of four Redis streams + per-user balance
//! values + the last id of `usage:detailed`. `report` (Task 7) consumes the
//! snapshot to produce window-delta aggregates.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use fred::interfaces::{KeysInterface, StreamsInterface};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::store::Store;
use crate::AppState;

/// In-memory table of active snapshots, keyed by `snapshot_id`.
#[derive(Default)]
pub struct SoakTable {
    inner: Mutex<HashMap<String, SoakSnapshot>>,
}

impl SoakTable {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn insert(&self, snap: SoakSnapshot) {
        let mut t = self.inner.lock().await;
        t.insert(snap.snapshot_id.clone(), snap);
    }
    pub async fn get(&self, id: &str) -> Option<SoakSnapshot> {
        let t = self.inner.lock().await;
        t.get(id).cloned()
    }
    pub async fn remove(&self, id: &str) -> Option<SoakSnapshot> {
        let mut t = self.inner.lock().await;
        t.remove(id)
    }
}

/// Snapshot captured at `POST /soak/window/start`.
///
/// XLEN fields are i64 to match `Store::xlen()` and the existing
/// `usage_drain::UsageStats` shape (the orchestrator already consumes i64
/// XLEN values from `/usage/stats`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SoakSnapshot {
    pub snapshot_id: String,
    pub t0_ms: i64,
    pub records_xlen: i64,
    pub dlq_xlen: i64,
    pub detailed_xlen: i64,
    pub events_entity_xlen: i64,
    pub detailed_last_id: String,
    pub balance_micros: HashMap<String, i64>,
}

#[derive(Debug, Deserialize)]
pub struct WindowStartReq {
    #[serde(default)]
    pub user_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WindowStartResp {
    pub snapshot_id: String,
    pub t0_ms: i64,
}

/// Allocate a fresh snapshot id. Hex of (millis<<16 | counter).
fn alloc_snapshot_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    format!("{:x}-{:x}", now, n)
}

pub async fn start(
    State(state): State<AppState>,
    Json(req): Json<WindowStartReq>,
) -> Result<Json<WindowStartResp>, (StatusCode, String)> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let snapshot_id = alloc_snapshot_id();
    let snap = capture_snapshot(&state.store, &snapshot_id, now_ms, &req.user_ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("snapshot: {e}")))?;

    state.soak.insert(snap).await;
    Ok(Json(WindowStartResp {
        snapshot_id,
        t0_ms: now_ms,
    }))
}

async fn capture_snapshot(
    store: &Arc<Store>,
    snapshot_id: &str,
    t0_ms: i64,
    user_ids: &[String],
) -> anyhow::Result<SoakSnapshot> {
    let records_xlen = store.xlen("usage:records").await.unwrap_or(0);
    let dlq_xlen = store.xlen("usage:records:dlq").await.unwrap_or(0);
    let detailed_xlen = store.xlen("usage:detailed").await.unwrap_or(0);
    let events_entity_xlen = store.xlen("events:entity").await.unwrap_or(0);

    let last_id: String = if detailed_xlen == 0 {
        "0-0".into()
    } else {
        let client = store.redis.next();
        let entries: Vec<(String, Vec<(String, String)>)> = client
            .xrevrange("usage:detailed", "+", "-", Some(1_u64))
            .await
            .unwrap_or_default();
        entries
            .into_iter()
            .next()
            .map(|(id, _)| id)
            .unwrap_or_else(|| "0-0".into())
    };

    let mut balance_micros = HashMap::with_capacity(user_ids.len());
    for u in user_ids {
        let key = format!("counter:user:{u}:balance_micro");
        let v: i64 = store.redis.next().get(&key).await.unwrap_or(0);
        balance_micros.insert(u.clone(), v);
    }

    Ok(SoakSnapshot {
        snapshot_id: snapshot_id.to_string(),
        t0_ms,
        records_xlen,
        dlq_xlen,
        detailed_xlen,
        events_entity_xlen,
        detailed_last_id: last_id,
        balance_micros,
    })
}

/// Pure decision: do the last `window` entries of `series` all match?
///
/// Returns `(quiescent, last_value)`. `last_value` is the most recent
/// observation for caller logging; defined to be 0 if `series` is empty.
pub fn quiescence_check(series: &[u64], window: usize) -> (bool, u64) {
    if series.len() < window {
        return (false, *series.last().unwrap_or(&0));
    }
    let tail = &series[series.len() - window..];
    let first = tail[0];
    (tail.iter().all(|&v| v == first), *series.last().unwrap())
}

/// Poll `XLEN usage:records` once per second; return when the last
/// `quiescence_s` samples match, or when `max_wait_s` elapses.
///
/// Returns `(quiescent_reached, observed_series)`. The series is the
/// full observation list — the caller writes it into the report's
/// `drain_observation_series` field.
pub async fn wait_for_quiescence(
    store: &Arc<Store>,
    quiescence_s: usize,
    max_wait_s: u64,
) -> (bool, Vec<u64>) {
    let mut series: Vec<u64> = Vec::with_capacity(max_wait_s as usize + 1);
    let deadline = Instant::now() + Duration::from_secs(max_wait_s);
    loop {
        let xlen_i64: i64 = store.xlen("usage:records").await.unwrap_or(0);
        let xlen = xlen_i64.max(0) as u64;
        series.push(xlen);
        let (quiescent, _) = quiescence_check(&series, quiescence_s);
        if quiescent {
            return (true, series);
        }
        if Instant::now() >= deadline {
            return (false, series);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

// ── Detailed-log spectrum ────────────────────────────────────────────────────

/// Spectrum view of `usage:detailed` entries, grouped by the fields the
/// gateway already emits. Missing fields are bucketed as `__absent__` —
/// no silent default per spec C4 (truthfulness).
#[derive(Default, Debug, Serialize)]
pub struct DetailedSpectrum {
    pub by_status: HashMap<String, u64>,
    pub by_error_code: HashMap<String, u64>,
    pub by_protocol: HashMap<String, u64>,
    pub by_shape: HashMap<String, u64>,
    pub by_cross_proto: HashMap<String, u64>,
}

pub fn aggregate_spectrum(entries: &[Vec<(String, String)>]) -> DetailedSpectrum {
    let mut out = DetailedSpectrum::default();
    for fields in entries {
        let get = |k: &str| -> Option<&str> {
            fields
                .iter()
                .find_map(|(fk, fv)| (fk == k).then_some(fv.as_str()))
        };

        bucket(&mut out.by_status, get("status"));
        bucket(
            &mut out.by_error_code,
            get("error_code").filter(|s| !s.is_empty()),
        );
        bucket(&mut out.by_protocol, get("protocol"));
        bucket(
            &mut out.by_shape,
            get("stream").map(|s| match s {
                "true" => "stream",
                "false" => "non_stream",
                other => other,
            }),
        );
        bucket(&mut out.by_cross_proto, get("protocol_conversion"));
    }
    out
}

fn bucket(map: &mut HashMap<String, u64>, value: Option<&str>) {
    let key = value.unwrap_or("__absent__");
    *map.entry(key.to_string()).or_insert(0) += 1;
}

// ── Window report ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct WindowReportQ {
    pub snapshot_id: String,
}

#[derive(Debug, Serialize)]
pub struct WindowReport {
    pub snapshot_id: String,
    pub window_ms: u64,
    pub drain_quiescent: bool,
    pub drain_observation_series: Vec<u64>,
    pub redis_streams: RedisStreamDeltas,
    pub balance_decrement_micros: HashMap<String, i64>,
    pub detailed_log_spectrum: DetailedSpectrum,
}

#[derive(Debug, Serialize)]
pub struct RedisStreamDeltas {
    pub usage_records_delta: u64,
    pub usage_records_dlq_delta: u64,
    pub usage_detailed_delta: u64,
    pub events_entity_delta: u64,
}

pub async fn report(
    State(state): State<AppState>,
    Query(q): Query<WindowReportQ>,
) -> Result<Json<WindowReport>, (StatusCode, String)> {
    let snap = state.soak.get(&q.snapshot_id).await.ok_or((
        StatusCode::NOT_FOUND,
        format!("snapshot_id not found: {}", q.snapshot_id),
    ))?;

    // Block until usage:records has been quiescent for 10s (max 60s).
    let (drain_quiescent, drain_observation_series) =
        wait_for_quiescence(&state.store, 10, 60).await;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let window_ms = (now_ms - snap.t0_ms).max(0) as u64;

    // Helper: i64 -> u64 saturate (XLEN values are always >= 0 in practice).
    let to_u64 = |v: i64| v.max(0) as u64;

    let records_xlen = to_u64(state.store.xlen("usage:records").await.unwrap_or(0));
    let dlq_xlen = to_u64(state.store.xlen("usage:records:dlq").await.unwrap_or(0));
    let detailed_xlen = to_u64(state.store.xlen("usage:detailed").await.unwrap_or(0));
    let events_entity_xlen = to_u64(state.store.xlen("events:entity").await.unwrap_or(0));

    // XRANGE usage:detailed (last_id +) — entries written during the window.
    // '(' prefix = exclusive lower bound per Redis docs; matches drain.rs:41.
    let client = state.store.redis.next();
    let after = format!("({}", snap.detailed_last_id);
    let xrange: Vec<(String, Vec<(String, String)>)> = client
        .xrange("usage:detailed", after, "+", None)
        .await
        .unwrap_or_default();
    let entries_only: Vec<Vec<(String, String)>> = xrange.into_iter().map(|(_, e)| e).collect();
    let spectrum = aggregate_spectrum(&entries_only);

    // Balance decrements: snapshot - current. Positive means money spent.
    let mut balance_decrement_micros = HashMap::with_capacity(snap.balance_micros.len());
    for (u, before) in &snap.balance_micros {
        let key = format!("counter:user:{u}:balance_micro");
        let after_val: i64 = client.get(&key).await.unwrap_or(0);
        balance_decrement_micros.insert(u.clone(), before - after_val);
    }

    // Free the snapshot — done with it.
    let _ = state.soak.remove(&q.snapshot_id).await;

    Ok(Json(WindowReport {
        snapshot_id: q.snapshot_id,
        window_ms,
        drain_quiescent,
        drain_observation_series,
        redis_streams: RedisStreamDeltas {
            usage_records_delta: records_xlen.saturating_sub(to_u64(snap.records_xlen)),
            usage_records_dlq_delta: dlq_xlen.saturating_sub(to_u64(snap.dlq_xlen)),
            usage_detailed_delta: detailed_xlen.saturating_sub(to_u64(snap.detailed_xlen)),
            events_entity_delta: events_entity_xlen.saturating_sub(to_u64(snap.events_entity_xlen)),
        },
        balance_decrement_micros,
        detailed_log_spectrum: spectrum,
    }))
}
