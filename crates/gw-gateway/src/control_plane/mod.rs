pub mod command_consumer;
pub mod state_writer;

use crate::config::ControlPlaneCfg;
use gw_health::KeyHealthStore;
use gw_routing::InFlightStore;
use gw_store::{redis_client::RedisCtx, EntityStore};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

pub struct ControlPlaneHandles {
    pub observed: Arc<ControlPlaneObserved>,
}

#[derive(Default)]
pub struct ControlPlaneObserved {
    pub last_command_stream_ms: AtomicU64,
    pub last_command_ack_ms: AtomicU64,
}

pub fn start(
    redis: RedisCtx,
    store: Arc<EntityStore>,
    health: Arc<KeyHealthStore>,
    in_flight: Arc<InFlightStore>,
    instance_id: String,
    cfg: &ControlPlaneCfg,
) -> ControlPlaneHandles {
    let observed = Arc::new(ControlPlaneObserved::default());
    let dynamic = Arc::new(state_writer::DynamicRuntimeCfg::new(
        cfg.runtime_push_interval_ms,
        cfg.snapshot_ttl_ms,
        cfg.command_block_ms,
    ));

    state_writer::StateWriter {
        redis: redis.clone(),
        health: health.clone(),
        in_flight,
        instance_id: instance_id.clone(),
        started_at_ms: state_writer::now_ms(),
        dynamic: dynamic.clone(),
        observed: observed.clone(),
        command_group: cfg.command_group.clone(),
        command_consumer: command_consumer::consumer_name(cfg, &instance_id),
    }
    .start();

    Arc::new(command_consumer::CommandConsumer {
        redis,
        store,
        health,
        instance_id: instance_id.clone(),
        group: command_consumer::group_name(&cfg.command_group, &instance_id),
        consumer: command_consumer::consumer_name(cfg, &instance_id),
        dynamic,
        observed: observed.clone(),
    })
    .start();

    ControlPlaneHandles { observed }
}
