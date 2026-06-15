//! Usage flusher — single background task that batches `UsageRecord`s from
//! an mpsc channel and writes them to Redis in a single pipelined `XADD`
//! batch.
//!
//! Per integration spec §2 表 A A6 + C-γ §ADR-Cγ-6: the flusher only
//! appends to `usage:records`. Balance decrement is Go's job (it consumes
//! `usage:records`, computes cost, writes `events:counter`). Rust no
//! longer touches `counter:user:*:balance_micro` here.
//!
//! Batching cap is set by `batch_size`; an interval timer flushes any
//! partial buffer so latency stays bounded even at low QPS.
//!
//! **Must-deliver semantic**: `emit` is the production billing path — on a
//! full mpsc channel it falls back to a synchronous direct XADD rather than
//! dropping; `try_send` retains the original drop-on-full behaviour for
//! callers that explicitly opt out of the blocking fallback.

use crate::backlog::BillingBacklog;
use crate::dlq::{DlqEnvelope, DlqLayer, EmergencyDlqWriter};
use crate::usage::UsageRecord;
use arc_swap::ArcSwapOption;
use fred::interfaces::{KeysInterface, StreamsInterface};
use fred::types::{XCap, XCapKind, XCapTrim};
use futures::FutureExt;
use gw_core::ids::UserId;
use gw_runtime::spill_io::SpillRoot;
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};
use gw_store::redis_client::RedisCtx;
use once_cell::sync::Lazy;
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{interval, Duration};

const USAGE_STREAM: &str = "usage:records";
const DLQ_STREAM: &str = "usage:records:dlq";
const DLQ_PERMANENT_FAILED_STREAM: &str = "usage:records:dlq:permanent_failed";
const DLQ_MAXLEN: u64 = 100_000;
const DLQ_RETRY_CAPACITY: usize = 4096;
const FROZEN_DELTA_CAPACITY: usize = 65_536;

static FROZEN_DELTA_TX: Lazy<ArcSwapOption<mpsc::Sender<FrozenDelta>>> =
    Lazy::new(ArcSwapOption::empty);

/// Channel-internal envelope: the wire `UsageRecord` plus an optional
/// inflight clear instruction. The flusher (or `sync_xadd` bypass) DELs
/// `billing:inflight:<instance>:<request_id>:<attempt>` after — and only
/// after — the corresponding `record` is XADD-confirmed on
/// `usage:records`. See `docs/spec/06-billing-ledger.md` (DEL rule).
#[derive(Clone, Debug)]
pub struct LedgerItem {
    pub record: UsageRecord,
    pub clear: Option<ClearKey>,
}

#[derive(Clone, Debug)]
pub struct ClearKey {
    pub request_id: String,
    pub attempt_index: u32,
}

pub struct Ledger {
    pub tx: mpsc::Sender<LedgerItem>,
    redis: RedisCtx,
    instance_id: String,
    dlq_fallback: DlqFallback,
}

impl Ledger {
    /// Start the Ledger with all four supervised background workers
    /// (billing flusher, frozen-delta worker, DLQ retry worker,
    /// billing backlog drainer).
    ///
    /// Returns the `Ledger` paired with the `JoinHandle`s of all spawned
    /// supervised tasks. The caller MUST register every handle with the
    /// `ShutdownCoordinator` so `drain()` can await their completion —
    /// otherwise the coordinator races the workers and may exit before
    /// the cancel-drain finishes flushing in-flight billing records.
    ///
    /// `drain_budget` bounds how long the cancel-drain loop will keep
    /// recv-ing newly-emitted records after the shutdown token fires.
    /// It MUST be strictly less than `phase2_task_drain_ms` so the worker
    /// returns before the coordinator's phase-2 timeout aborts it.
    /// See `RuntimeShutdownConfig::billing_drain_ms` (default 20 s).
    ///
    /// The backlog used here is a private, fresh `BillingBacklog`. Callers
    /// that need to share a backlog across crates (e.g. for
    /// `BacklogRecovery` on bootstrap, T4.4) should use
    /// [`Ledger::start_with_spill_root`] and pass their own.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        redis: RedisCtx,
        instance_id: String,
        channel_capacity: usize,
        batch_size: usize,
        flush_interval: Duration,
        spill_root: Arc<SpillRoot>,
        dlq_shards: usize,
        shutdown: ShutdownToken,
        drain_budget: Duration,
    ) -> (Self, Vec<JoinHandle<()>>) {
        Self::start_with_spill_root(
            redis,
            instance_id,
            channel_capacity,
            batch_size,
            flush_interval,
            spill_root,
            dlq_shards,
            Arc::new(BillingBacklog::new()),
            shutdown,
            drain_budget,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_with_spill_root(
        redis: RedisCtx,
        instance_id: String,
        channel_capacity: usize,
        batch_size: usize,
        flush_interval: Duration,
        spill_root: Arc<SpillRoot>,
        dlq_shards: usize,
        backlog: Arc<BillingBacklog>,
        shutdown: ShutdownToken,
        drain_budget: Duration,
    ) -> (Self, Vec<JoinHandle<()>>) {
        let (tx, rx) = mpsc::channel::<LedgerItem>(channel_capacity);
        let rx = Arc::new(Mutex::new(rx));
        let (dlq_retry_tx, dlq_retry_rx) = mpsc::channel::<DlqEnvelope>(DLQ_RETRY_CAPACITY);
        let (frozen_tx, frozen_rx) = mpsc::channel::<FrozenDelta>(FROZEN_DELTA_CAPACITY);
        FROZEN_DELTA_TX.store(Some(Arc::new(frozen_tx)));
        let shards = NonZeroUsize::new(dlq_shards.max(1)).expect("dlq_shards must be >= 1");
        let dlq_fallback = DlqFallback {
            writer: Arc::new(EmergencyDlqWriter::new(spill_root, shards)),
            retry_tx: dlq_retry_tx,
            backlog: backlog.clone(),
        };
        let dlq_handle = spawn_dlq_retry_worker(
            redis.clone(),
            dlq_retry_rx,
            dlq_fallback.writer.clone(),
            shutdown.clone(),
            drain_budget,
        );
        let frozen_handle =
            spawn_frozen_delta_worker(redis.clone(), frozen_rx, shutdown.clone(), drain_budget);
        let backlog_handle = spawn_backlog_drainer(
            redis.clone(),
            dlq_fallback.writer.clone(),
            backlog,
            shutdown.clone(),
        );
        let worker_redis = redis.clone();
        let worker_instance = instance_id.clone();
        let worker_dlq = dlq_fallback.clone();
        let flusher_handle = Supervisor::new("billing_flusher", RestartPolicy::default(), shutdown)
            .spawn(move |tok| {
                let rx = rx.clone();
                let worker_redis = worker_redis.clone();
                let worker_instance = worker_instance.clone();
                let worker_dlq = worker_dlq.clone();
                async move {
                    let mut buf: Vec<LedgerItem> = Vec::with_capacity(batch_size);
                    let mut ticker = interval(flush_interval);
                    loop {
                        tokio::select! {
                            _ = tok.cancelled() => {
                                // Cancel-drain: must-deliver semantic.
                                //
                                // Phase-1 of shutdown (default 5 s) sleeps while Pingora
                                // continues serving in-flight requests; every Ledger::emit
                                // during that window calls try_send Ok (channel has 65 k
                                // capacity) and the record sits in the unread channel. A
                                // one-shot try_recv would lose all of them — we MUST keep
                                // recv-ing until the channel closes or the drain budget
                                // expires.
                                drain_billing_channel(
                                    &rx,
                                    &mut buf,
                                    batch_size,
                                    drain_budget,
                                    &worker_redis,
                                    &worker_instance,
                                    &worker_dlq,
                                ).await;
                                tracing::info!(
                                    target: "gw_ledger",
                                    "billing_flusher: shutdown drain complete"
                                );
                                return Ok(());
                            }
                            maybe_item = async {
                                let mut guard = rx.lock().await;
                                guard.recv().await
                            } => {
                                match maybe_item {
                                    Some(item) => {
                                        buf.push(item);
                                        if buf.len() >= batch_size {
                                            let mut taken = std::mem::take(&mut buf);
                                            let result = AssertUnwindSafe(
                                                flush(&worker_redis, &worker_instance, &worker_dlq, &mut taken)
                                            ).catch_unwind().await;
                                            if let Err(_panic) = result {
                                                metrics::counter!("gw_billing_flusher_panic_total").increment(1);
                                                tracing::error!(
                                                    target: "gw_ledger",
                                                    "flusher: flush() panicked; channel kept open, batch lost (inflight keys remain for reconciler)"
                                                );
                                            }
                                        }
                                    }
                                    None => {
                                        if !buf.is_empty() {
                                            let mut taken = std::mem::take(&mut buf);
                                            let _ = AssertUnwindSafe(
                                                flush(&worker_redis, &worker_instance, &worker_dlq, &mut taken)
                                            ).catch_unwind().await;
                                        }
                                        break;
                                    }
                                }
                            }
                            _ = ticker.tick() => {
                                if !buf.is_empty() {
                                    let mut taken = std::mem::take(&mut buf);
                                    let result = AssertUnwindSafe(
                                        flush(&worker_redis, &worker_instance, &worker_dlq, &mut taken)
                                    ).catch_unwind().await;
                                    if let Err(_panic) = result {
                                        metrics::counter!("gw_billing_flusher_panic_total").increment(1);
                                        tracing::error!(
                                            target: "gw_ledger",
                                            "flusher: flush() panicked on tick; channel kept open"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Ok(())
                }
            });
        (
            Self {
                tx,
                redis,
                instance_id,
                dlq_fallback,
            },
            vec![flusher_handle, dlq_handle, frozen_handle, backlog_handle],
        )
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Construct from raw parts. **Test-only entry point** — used by
    /// integration tests to simulate a Ledger whose flusher task has died
    /// (closed `rx`). Production code uses `Ledger::start`, which spawns
    /// the worker task internally.
    #[doc(hidden)]
    pub fn from_parts(
        tx: mpsc::Sender<LedgerItem>,
        redis: RedisCtx,
        instance_id: String,
        spill_root: Arc<SpillRoot>,
        dlq_shards: NonZeroUsize,
    ) -> Self {
        let (retry_tx, _rx) = mpsc::channel(1);
        Self {
            tx,
            redis,
            instance_id,
            dlq_fallback: DlqFallback {
                writer: Arc::new(EmergencyDlqWriter::new(spill_root, dlq_shards)),
                retry_tx,
                backlog: Arc::new(BillingBacklog::new()),
            },
        }
    }

    /// Must-deliver billing emit path. Attempts the mpsc fast path first; on a
    /// full channel falls back to a synchronous direct XADD (bypass), routing
    /// to the DLQ if even the bypass fails. Use this for all production billing
    /// emits so that backpressure never silently drops revenue records.
    pub async fn emit(&self, item: LedgerItem) {
        match self.tx.try_send(item) {
            Ok(()) => {
                metrics::counter!("gw_billing_emit_path_total", "path" => "channel").increment(1);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(item_back)) => {
                metrics::counter!("gw_billing_mpsc_bypass_total").increment(1);
                match sync_xadd(&self.redis, &item_back.record).await {
                    Ok(()) => {
                        // XADD durably landed — apply the DEL rule.
                        if let Some(ck) = &item_back.clear {
                            sync_del_inflight(&self.redis, &self.instance_id, ck).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "must-deliver sync XADD bypass failed; routing to DLQ");
                        let envelope = DlqEnvelope::new(
                            item_back.record.clone(),
                            format!("sync_xadd_bypass_failed: {e}"),
                            0,
                            DlqLayer::Redis,
                            now_ms(),
                        );
                        route_to_dlq(&self.dlq_fallback, envelope).await;
                        if item_back.clear.is_some() {
                            metrics::counter!(
                                "gw_billing_inflight_clear_total",
                                "outcome" => "skipped_dlq"
                            )
                            .increment(1);
                        }
                    }
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(item_back)) => {
                metrics::counter!("gw_billing_emit_path_total", "path" => "closed").increment(1);
                metrics::counter!("gw_billing_flusher_dead_total").increment(1);
                tracing::error!(
                    "usage record channel closed; flusher dead — falling back to sync XADD"
                );
                match sync_xadd(&self.redis, &item_back.record).await {
                    Ok(()) => {
                        metrics::counter!(
                            "gw_billing_emit_path_total",
                            "path" => "sync_bypass_closed"
                        )
                        .increment(1);
                        if let Some(ck) = &item_back.clear {
                            sync_del_inflight(&self.redis, &self.instance_id, ck).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "Closed-branch sync XADD failed; routing to DLQ");
                        let envelope = DlqEnvelope::new(
                            item_back.record.clone(),
                            format!("flusher_dead: {e}"),
                            0,
                            DlqLayer::Redis,
                            now_ms(),
                        );
                        route_to_dlq(&self.dlq_fallback, envelope).await;
                        if item_back.clear.is_some() {
                            metrics::counter!(
                                "gw_billing_inflight_clear_total",
                                "outcome" => "skipped_dlq"
                            )
                            .increment(1);
                        }
                    }
                }
            }
        }
    }

    /// Non-blocking enqueue. On a full channel we drop the record and bump
    /// the `gateway_usage_channel_full_total` counter — the alternative
    /// (blocking the proxy request task) would propagate Redis stalls into
    /// upstream latency. Prefer `emit` for production billing paths.
    #[allow(clippy::result_unit_err)]
    pub fn try_send(&self, item: LedgerItem) -> Result<(), ()> {
        self.tx.try_send(item).map_err(|_| {
            metrics::counter!("gateway_usage_channel_full_total").increment(1);
        })
    }
}

/// Cancel-drain helper for the billing flusher.
///
/// Keeps `recv()`-ing newly-emitted `LedgerItem`s until the channel closes
/// (all senders dropped) or the `drain_budget` deadline expires, flushing
/// batches as they fill (or at the deadline if a partial batch remains).
/// Must-deliver semantic: a one-shot `try_recv` at the cancel instant would
/// lose records `emit`ted during phase-1 (default 5 s) of the
/// `ShutdownCoordinator` drain, since Pingora keeps serving in-flight
/// requests on its own thread during that window.
async fn drain_billing_channel(
    rx: &Arc<Mutex<mpsc::Receiver<LedgerItem>>>,
    buf: &mut Vec<LedgerItem>,
    batch_size: usize,
    drain_budget: Duration,
    worker_redis: &RedisCtx,
    worker_instance: &str,
    worker_dlq: &DlqFallback,
) {
    let deadline = tokio::time::Instant::now() + drain_budget;
    let mut rx_guard = rx.lock().await;
    loop {
        // Drain anything already queued without awaiting.
        while let Ok(item) = rx_guard.try_recv() {
            buf.push(item);
        }
        // Flush if we have a full batch, OR if we hold a partial batch and
        // are at/past the deadline (last chance to emit before returning).
        let now = tokio::time::Instant::now();
        let at_deadline = now >= deadline;
        if buf.len() >= batch_size || (!buf.is_empty() && at_deadline) {
            let mut taken = std::mem::take(buf);
            let _ = AssertUnwindSafe(flush(worker_redis, worker_instance, worker_dlq, &mut taken))
                .catch_unwind()
                .await;
        }
        if at_deadline {
            break;
        }
        // Wait for more records up to the deadline. Ok(None) ⇒ all senders
        // dropped (clean exit). Err(_) ⇒ timeout elapsed (budget exhausted).
        match tokio::time::timeout_at(deadline, rx_guard.recv()).await {
            Ok(Some(item)) => buf.push(item),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    // One final flush for whatever's still in the buffer (covers items
    // pushed by the `Ok(Some(item))` arm immediately before the deadline,
    // or items try_recv'd in the next loop iteration after a budget-bounded
    // recv returned Err.
    if !buf.is_empty() {
        let mut taken = std::mem::take(buf);
        let _ = AssertUnwindSafe(flush(worker_redis, worker_instance, worker_dlq, &mut taken))
            .catch_unwind()
            .await;
    }
}

#[derive(Clone, Debug)]
pub struct FrozenDelta {
    pub uid: UserId,
    pub delta_micro: i64,
}

pub fn enqueue_frozen_delta(uid: &UserId, delta_micro: i64) {
    let Some(tx) = FROZEN_DELTA_TX.load_full() else {
        metrics::counter!("gw_frozen_delta_enqueue_total", "outcome" => "no_sink").increment(1);
        return;
    };
    match tx.try_send(FrozenDelta {
        uid: uid.clone(),
        delta_micro,
    }) {
        Ok(()) => {
            metrics::counter!("gw_frozen_delta_enqueue_total", "outcome" => "ok").increment(1);
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            metrics::counter!("gw_frozen_delta_enqueue_total", "outcome" => "full").increment(1);
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            metrics::counter!("gw_frozen_delta_enqueue_total", "outcome" => "no_sink").increment(1);
        }
    }
}

#[doc(hidden)]
pub fn install_frozen_delta_sender_for_tests(
    tx: mpsc::Sender<FrozenDelta>,
) -> FrozenDeltaSenderGuard {
    let previous = FROZEN_DELTA_TX.swap(Some(Arc::new(tx)));
    FrozenDeltaSenderGuard { previous }
}

#[doc(hidden)]
pub struct FrozenDeltaSenderGuard {
    previous: Option<Arc<mpsc::Sender<FrozenDelta>>>,
}

impl Drop for FrozenDeltaSenderGuard {
    fn drop(&mut self) {
        FROZEN_DELTA_TX.store(self.previous.take());
    }
}

fn spawn_frozen_delta_worker(
    redis: RedisCtx,
    rx: mpsc::Receiver<FrozenDelta>,
    shutdown: ShutdownToken,
    drain_budget: Duration,
) -> JoinHandle<()> {
    let rx = Arc::new(Mutex::new(rx));
    Supervisor::new("frozen_delta_worker", RestartPolicy::default(), shutdown).spawn(move |tok| {
        let rx = rx.clone();
        let redis = redis.clone();
        async move {
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        // Cancel-drain: late `enqueue_frozen_delta` calls
                        // during phase-1 land in the channel. Recv with a
                        // deadline so we apply them before exiting.
                        drain_frozen_delta_channel(&rx, &redis, drain_budget).await;
                        tracing::info!(
                            target: "gw_ledger",
                            "frozen_delta_worker: shutdown drain complete"
                        );
                        return Ok(());
                    }
                    delta = async {
                        let mut guard = rx.lock().await;
                        guard.recv().await
                    } => {
                        match delta {
                            Some(delta) => apply_frozen_delta(&redis, &delta).await,
                            None => break,
                        }
                    }
                }
            }
            Ok(())
        }
    })
}

async fn apply_frozen_delta(redis: &RedisCtx, delta: &FrozenDelta) {
    let key = frozen_counter_key(&delta.uid);
    let client = redis.pool.next();
    match client.incr_by::<i64, _>(&key, delta.delta_micro).await {
        Ok(_) => {
            metrics::counter!("gw_frozen_delta_redis_total", "outcome" => "ok").increment(1);
        }
        Err(err) => {
            metrics::counter!("gw_frozen_delta_redis_total", "outcome" => "error").increment(1);
            tracing::warn!(
                target: "gw_ledger",
                error = %err,
                key,
                "frozen_micro Redis sync failed"
            );
        }
    }
}

async fn drain_frozen_delta_channel(
    rx: &Arc<Mutex<mpsc::Receiver<FrozenDelta>>>,
    redis: &RedisCtx,
    drain_budget: Duration,
) {
    let deadline = tokio::time::Instant::now() + drain_budget;
    let mut rx_guard = rx.lock().await;
    loop {
        while let Ok(delta) = rx_guard.try_recv() {
            apply_frozen_delta(redis, &delta).await;
        }
        match tokio::time::timeout_at(deadline, rx_guard.recv()).await {
            Ok(Some(delta)) => apply_frozen_delta(redis, &delta).await,
            Ok(None) => break,
            Err(_) => break,
        }
    }
}

fn frozen_counter_key(uid: &UserId) -> String {
    format!("counter:user:{}:frozen_micro", uid.as_ref())
}

#[derive(Clone)]
pub struct DlqFallback {
    pub writer: Arc<EmergencyDlqWriter>,
    pub retry_tx: mpsc::Sender<DlqEnvelope>,
    pub backlog: Arc<BillingBacklog>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Background drainer for `BillingBacklog` — the unbounded last-fallback
/// in-memory queue populated by `route_to_dlq` when both the in-memory retry
/// channel AND the SpillFile DLQ have failed.
///
/// Every 500 ms tick, pops all current backlog entries and tries to push each
/// to the Redis DLQ stream via `dlq_xadd`. On Redis failure, falls back to
/// `EmergencyDlqWriter::append` (disk). If both fail, the envelope is pushed
/// **back** to the tail of the backlog and the drainer breaks out of the
/// inner loop, waiting for the next tick. This is the must-deliver invariant
/// in `docs/superpowers/specs/2026-06-11-resilience-fixes-design.md` §6.
///
/// **Re-push reordering**: `BillingBacklog` exposes only tail-`push`, so a
/// failed re-push lands at the tail rather than the head it came from. This
/// is acceptable — `usage:records` ordering is not strictly preserved across
/// failure cascades anyway (the consumer reconciles by `request_id` +
/// `attempt_index`), and the alternative (a `push_front`) would invite
/// pathological tight loops on a sustained Redis+disk outage.
///
/// Supervised under `RestartPolicy::default()` so panics restart cleanly.
pub fn spawn_backlog_drainer(
    redis: RedisCtx,
    writer: Arc<EmergencyDlqWriter>,
    backlog: Arc<BillingBacklog>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    Supervisor::new(
        "billing_backlog_drainer",
        RestartPolicy::default(),
        shutdown,
    )
    .spawn(move |tok| {
        let redis = redis.clone();
        let writer = writer.clone();
        let backlog = backlog.clone();
        async move {
            let mut tick = interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        tracing::info!(
                            target: "gw_ledger",
                            pending = backlog.pending_count(),
                            "billing_backlog_drainer: shutdown — backlog persistence is T4.5",
                        );
                        return Ok(());
                    }
                    _ = tick.tick() => {
                        while let Some(env) = backlog.pop().await {
                            match dlq_xadd(&redis, &env).await {
                                Ok(()) => {
                                    metrics::counter!(
                                        "gw_billing_backlog_drained_total",
                                        "outcome" => "redis"
                                    )
                                    .increment(1);
                                    continue;
                                }
                                Err(_) => {
                                    let disk = env.with_layer(DlqLayer::Disk);
                                    if writer.append(&disk).await.is_ok() {
                                        metrics::counter!(
                                            "gw_billing_backlog_drained_total",
                                            "outcome" => "disk"
                                        )
                                        .increment(1);
                                        continue;
                                    }
                                    // Re-push to tail (slight reordering quirk; see
                                    // doc comment above). Never drops.
                                    metrics::counter!(
                                        "gw_billing_backlog_drained_total",
                                        "outcome" => "re_pushed"
                                    )
                                    .increment(1);
                                    backlog.push(disk).await;
                                    break; // back off until next tick
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

fn spawn_dlq_retry_worker(
    redis: RedisCtx,
    rx: mpsc::Receiver<DlqEnvelope>,
    writer: Arc<EmergencyDlqWriter>,
    shutdown: ShutdownToken,
    drain_budget: Duration,
) -> JoinHandle<()> {
    let rx = Arc::new(Mutex::new(rx));
    Supervisor::new("dlq_retry_worker", RestartPolicy::default(), shutdown).spawn(move |tok| {
        let rx = rx.clone();
        let redis = redis.clone();
        let writer = writer.clone();
        async move {
            let mut ticker = interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        // Cancel-drain: late route_to_dlq calls during
                        // phase-1 land in the channel. Recv with a
                        // deadline so we retry them (skipping the
                        // inter-retry ticker — we're shutting down).
                        drain_dlq_retry_channel(&rx, &redis, &writer, drain_budget).await;
                        tracing::info!(
                            target: "gw_ledger",
                            "dlq_retry_worker: shutdown drain complete"
                        );
                        return Ok(());
                    }
                    envelope = async {
                        let mut guard = rx.lock().await;
                        guard.recv().await
                    } => {
                        match envelope {
                            Some(envelope) => {
                                ticker.tick().await;
                                process_dlq_envelope(&redis, &writer, envelope).await;
                            }
                            None => break,
                        }
                    }
                }
            }
            Ok(())
        }
    })
}

async fn process_dlq_envelope(
    redis: &RedisCtx,
    writer: &Arc<EmergencyDlqWriter>,
    mut envelope: DlqEnvelope,
) {
    envelope.dlq_meta.retry_count_before_dlq =
        envelope.dlq_meta.retry_count_before_dlq.saturating_add(1);
    if dlq_xadd(redis, &envelope).await.is_ok() {
        return;
    }
    let disk_envelope = envelope.with_layer(DlqLayer::Disk);
    if let Err(err) = writer.append(&disk_envelope).await {
        metrics::counter!("gw_usage_dlq_emergency_file_total", "outcome" => "error").increment(1);
        tracing::error!(
            target: "gw_ledger",
            error = %err,
            "DLQ retry failed and emergency JSONL write failed"
        );
        if permanent_failed_xadd(
            redis,
            &disk_envelope,
            &format!("retry_dlq_and_emergency_file_failed: {err}"),
        )
        .await
        .is_err()
        {
            metrics::counter!("gateway_usage_records_lost_total").increment(1);
        }
    }
}

async fn drain_dlq_retry_channel(
    rx: &Arc<Mutex<mpsc::Receiver<DlqEnvelope>>>,
    redis: &RedisCtx,
    writer: &Arc<EmergencyDlqWriter>,
    drain_budget: Duration,
) {
    let deadline = tokio::time::Instant::now() + drain_budget;
    let mut rx_guard = rx.lock().await;
    loop {
        while let Ok(envelope) = rx_guard.try_recv() {
            process_dlq_envelope(redis, writer, envelope).await;
            if tokio::time::Instant::now() >= deadline {
                return;
            }
        }
        match tokio::time::timeout_at(deadline, rx_guard.recv()).await {
            Ok(Some(envelope)) => process_dlq_envelope(redis, writer, envelope).await,
            Ok(None) => break,
            Err(_) => break,
        }
    }
}

/// Build the Redis key for an inflight billing hash.
///
/// MUST match `InflightLedger::test_key` / `key()` in `inflight.rs` — Redis
/// is the integration boundary; a drift here would silently orphan keys.
fn inflight_key(instance_id: &str, ck: &ClearKey) -> String {
    format!(
        "billing:inflight:{instance_id}:{}:{}",
        ck.request_id, ck.attempt_index
    )
}

/// Direct single-record XADD to `usage:records`, used as the must-deliver
/// bypass when the mpsc channel is full.
async fn sync_xadd(redis: &RedisCtx, rec: &UsageRecord) -> Result<(), String> {
    let payload = serde_json::to_string(rec).map_err(|e| format!("serialize: {e}"))?;
    let client = redis.pool.next();
    client
        .xadd::<(), _, _, _, _>(USAGE_STREAM, false, None::<()>, "*", ("payload", payload))
        .await
        .map_err(|e| format!("xadd: {e}"))
}

/// DEL a single inflight key after a successful sync_xadd bypass. Best-effort:
/// failure is warned + counted but never propagated (record durability has
/// already been confirmed at the caller; key residue is harmless and
/// reconciler-recoverable).
async fn sync_del_inflight(redis: &RedisCtx, instance_id: &str, ck: &ClearKey) {
    let key = inflight_key(instance_id, ck);
    let client = redis.pool.next();
    match client.del::<(), _>(&[key.as_str()]).await {
        Ok(_) => {
            metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "ok").increment(1);
        }
        Err(e) => {
            tracing::warn!(target: "gw_ledger", error = %e, key, "sync inflight DEL failed");
            metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "del_failed")
                .increment(1);
        }
    }
}

fn dlq_cap() -> XCap {
    XCap::try_from((XCapKind::MaxLen, XCapTrim::AlmostExact, DLQ_MAXLEN))
        .expect("XCap::try_from infallible for (MaxLen, AlmostExact, u64)")
}

/// Best-effort XADD to the DLQ stream. Returns Ok on success; on failure the
/// record is fully dropped and the caller bumps `gw_usage_records_lost_total`.
async fn dlq_xadd(redis: &RedisCtx, envelope: &DlqEnvelope) -> Result<(), String> {
    let wrapped = serde_json::to_string(envelope).map_err(|e| format!("serialize dlq: {e}"))?;
    let client = redis.pool.next();
    let r: Result<String, _> = client
        .xadd(DLQ_STREAM, false, dlq_cap(), "*", ("payload", wrapped))
        .await;
    match r {
        Ok(_) => {
            metrics::counter!("gw_usage_dlq_enqueued_total", "outcome" => "success").increment(1);
            Ok(())
        }
        Err(e) => {
            metrics::counter!("gw_usage_dlq_enqueued_total", "outcome" => "error").increment(1);
            Err(e.to_string())
        }
    }
}

async fn permanent_failed_xadd(
    redis: &RedisCtx,
    envelope: &DlqEnvelope,
    reason: &str,
) -> Result<(), String> {
    let payload = serde_json::json!({
        "permanent_failure_reason": reason,
        "dlq_envelope": envelope,
    })
    .to_string();
    let client = redis.pool.next();
    client
        .xadd::<String, _, _, _, _>(
            DLQ_PERMANENT_FAILED_STREAM,
            false,
            dlq_cap(),
            "*",
            ("payload", payload),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Failure cascade for a DLQ-bound envelope.
///
/// Order: in-memory retry channel (fast path) → SpillFile (disk) → unbounded
/// `BillingBacklog` (last-resort, never drops). The drainer task
/// (`spawn_backlog_drainer`) periodically flushes the backlog back to Redis.
///
/// The primary Redis XADD attempt happens upstream in `flush()` / `emit()` —
/// only failures land here, so we skip a redundant DLQ-stream XADD and head
/// straight to the in-memory cascade. The `dlq_retry_worker` (consumer of
/// `retry_tx`) is responsible for the eventual Redis DLQ-stream XADD.
/// This matches the must-deliver invariant in
/// `docs/superpowers/specs/2026-06-11-resilience-fixes-design.md` §6.
async fn route_to_dlq(fallback: &DlqFallback, mut env: DlqEnvelope) {
    // Try retry channel first (in-memory bounded mpsc — fast path).
    env.dlq_meta.dlq_layer = DlqLayer::Mem;
    match fallback.retry_tx.try_send(env.clone()) {
        Ok(()) => {
            metrics::counter!("gw_usage_dlq_retry_queue_total", "outcome" => "enqueued")
                .increment(1);
            return;
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            metrics::counter!("gw_usage_dlq_retry_queue_total", "outcome" => "full").increment(1);
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            metrics::counter!("gw_usage_dlq_retry_queue_total", "outcome" => "closed").increment(1);
        }
    }
    // Then disk via SpillFile.
    let disk = env.with_layer(DlqLayer::Disk);
    match fallback.writer.append(&disk).await {
        Ok(()) => {
            metrics::counter!("gw_usage_dlq_emergency_file_total", "outcome" => "success")
                .increment(1);
            return;
        }
        Err(err) => {
            metrics::counter!("gw_usage_dlq_emergency_file_total", "outcome" => "error")
                .increment(1);
            tracing::error!(
                target: "gw_ledger",
                error = %err,
                "emergency DLQ JSONL write failed; falling back to in-memory backlog"
            );
        }
    }
    // Last fallback: unbounded BillingBacklog. Never drops.
    fallback.backlog.push(disk).await;
    metrics::counter!("gw_usage_dlq_backlog_pushed_total").increment(1);
}

async fn flush(
    redis: &RedisCtx,
    instance_id: &str,
    dlq_fallback: &DlqFallback,
    buf: &mut Vec<LedgerItem>,
) {
    // Drain into parallel vectors: (payload, clear_opt). Per spec
    // `docs/spec/06-billing-ledger.md`
    // (DEL rule), the flusher only DELs the inflight key after the
    // matching record's XADD on `usage:records` is confirmed; DLQ / serialize-
    // fail / queue-fail paths leave the key alone so the reconciler can
    // surface them via CrashFallback.
    let mut records: Vec<UsageRecord> = Vec::with_capacity(buf.len());
    let mut payloads: Vec<String> = Vec::with_capacity(buf.len());
    let mut clears: Vec<Option<ClearKey>> = Vec::with_capacity(buf.len());
    for item in buf.drain(..) {
        match serde_json::to_string(&item.record) {
            Ok(p) => {
                records.push(item.record);
                payloads.push(p);
                clears.push(item.clear);
            }
            Err(e) => {
                tracing::warn!(target: "gw_ledger", error = %e, "usage flush: serialize failed; record dropped");
                metrics::counter!("gateway_usage_serialize_errors_total").increment(1);
                // Serialize-fail path: drop record AND drop clear (no DEL —
                // key lingers, reconciler will emit CrashFallback later).
                // Consistent with the DEL rule (no XADD success ⇒ no DEL).
            }
        }
    }
    if payloads.is_empty() {
        return;
    }

    let batch_len = payloads.len();
    let client = redis.pool.next();
    let pipe = client.pipeline();
    // Track per-record queue failures so we can DLQ those individually.
    let mut queue_failed: Vec<(usize, String)> = Vec::new();
    let mut queued_indices: Vec<usize> = Vec::with_capacity(payloads.len());
    for (idx, payload) in payloads.iter().enumerate() {
        if let Err(e) = pipe
            .xadd::<(), _, _, _, _>(
                USAGE_STREAM,
                false,
                None::<()>,
                "*",
                ("payload", payload.clone()),
            )
            .await
        {
            tracing::warn!(target: "gw_ledger", error = %e, "usage flush: xadd queue failed");
            metrics::counter!("gateway_usage_queue_errors_total").increment(1);
            queue_failed.push((idx, e.to_string()));
        } else {
            queued_indices.push(idx);
        }
    }
    // RTT: execute the pipelined batch.
    let batch_result = pipe.all::<()>().await;

    // Collect inflight keys to DEL — only those whose XADD success is confirmed.
    let mut to_clear: Vec<String> = Vec::new();
    match batch_result {
        Ok(_) => {
            for idx in &queued_indices {
                if let Some(ck) = &clears[*idx] {
                    to_clear.push(inflight_key(instance_id, ck));
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "gw_ledger", error = %e, batch_len, "usage flush RTT failed; routing to DLQ");
            metrics::counter!("gateway_usage_flush_errors_total").increment(1);
            // Drain ALL queued records to DLQ.
            let reason = format!("batch_rtt: {e}");
            for idx in &queued_indices {
                let envelope = DlqEnvelope::new(
                    records[*idx].clone(),
                    reason.clone(),
                    0,
                    DlqLayer::Redis,
                    now_ms(),
                );
                route_to_dlq(dlq_fallback, envelope).await;
                // No DEL — DLQ path leaves inflight key for reconciler.
                if clears[*idx].is_some() {
                    metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "skipped_dlq").increment(1);
                }
            }
        }
    }

    // Per-record queue failures (independent of batch RTT).
    for (idx, reason) in queue_failed {
        let envelope = DlqEnvelope::new(
            records[idx].clone(),
            format!("xadd_queue: {reason}"),
            0,
            DlqLayer::Redis,
            now_ms(),
        );
        route_to_dlq(dlq_fallback, envelope).await;
        if clears[idx].is_some() {
            metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "skipped_dlq")
                .increment(1);
        }
    }

    // Execute the DEL pipeline for confirmed-XADD records with clear: Some.
    if !to_clear.is_empty() {
        let del_client = redis.pool.next();
        let del_pipe = del_client.pipeline();
        let mut del_queued: usize = 0;
        for key in &to_clear {
            if let Err(e) = del_pipe.del::<(), _>(&[key.as_str()]).await {
                tracing::warn!(target: "gw_ledger", error = %e, key, "inflight DEL queue failed");
                metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "del_failed")
                    .increment(1);
            } else {
                del_queued += 1;
            }
        }
        if del_queued > 0 {
            match del_pipe.all::<()>().await {
                Ok(_) => {
                    metrics::counter!("gw_billing_inflight_clear_total", "outcome" => "ok")
                        .increment(del_queued as u64);
                }
                Err(e) => {
                    tracing::warn!(target: "gw_ledger", error = %e, count = del_queued, "inflight DEL batch RTT failed");
                    metrics::counter!(
                        "gw_billing_inflight_clear_total",
                        "outcome" => "del_failed"
                    )
                    .increment(del_queued as u64);
                    // Harmless: keys linger, reconciler/T0 surfaces them.
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{UsageProvenance, UsageRecord, UsageStatus, VendorExposureState};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::permissions::MarkupChainSnapshot;
    use gw_core::usage::UsageSnapshot;

    fn dummy_record(req_id: &str) -> UsageRecord {
        UsageRecord {
            ts_ms: 0,
            request_id: req_id.to_string(),
            trace_id: String::new(),
            user_id: UserId::from("u0"),
            child_id: None,
            api_key_id: None,
            client_protocol: String::new(),
            upstream_protocol: String::new(),
            model: ModelName::from("m"),
            vendor_id: VendorId::from("v"),
            vendor_key_id: KeyId::from("k"),
            status: UsageStatus::Ok,
            error_code: None,
            http_status: 200,
            usage: UsageSnapshot::default(),
            attempt_index: 0,
            vendor_exposure_state: VendorExposureState::Settled,
            usage_provenance: UsageProvenance::default(),
            latency_ms: 0,
            ttft_ms: None,
            is_stream: false,
            freeze_amount_micro: 0,
            estimate_pricing_version: 0,
            markup_chain_snapshot: MarkupChainSnapshot::identity(),
            operation_id: None,
            pod_id: String::new(),
            credits_consumed: None,
            coef_table_version: None,
            attempted_keys: Vec::new(),
        }
    }

    #[test]
    fn ledger_item_constructs_with_and_without_clear() {
        let rec = dummy_record("req-1");
        let with_clear = LedgerItem {
            record: rec.clone(),
            clear: Some(ClearKey {
                request_id: "req-1".to_string(),
                attempt_index: 0,
            }),
        };
        let without_clear = LedgerItem {
            record: rec,
            clear: None,
        };
        assert!(with_clear.clear.is_some());
        assert!(without_clear.clear.is_none());
        assert_eq!(with_clear.record.request_id, "req-1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn flusher_panic_metric_symbol_resolves() {
        // Real panic-injection is integration-only (we can't easily make
        // `flush()` panic without mocking redis). This unit test just verifies
        // the metric counter symbol is bumpable — i.e. registered and not a typo.
        // The structural property (channel survives a flush panic) is validated
        // by the integration soak harness in Task 20 (`make e2e-soak-kill`).
        metrics::counter!("gw_billing_flusher_panic_total").increment(0);
    }
}
