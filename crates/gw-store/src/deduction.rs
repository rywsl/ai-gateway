//! Three-tier deduction state + per-user circuit breaker (ADR-021).
//!
//! `DeductionState` lives in process memory (in-process counters), with periodic
//! flush to Redis via the ledger pipeline. `UserCircuit` short-circuits requests
//! when balance reaches zero or the user is banned.

use dashmap::DashMap;
use gw_core::ids::UserId;
use once_cell::sync::Lazy;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Instant;

#[derive(Debug)]
pub struct DeductionState {
    pub base_micro: AtomicI64,
    pub deducted_micro: AtomicI64,
    pub frozen_micro: AtomicI64,
}

impl DeductionState {
    /// `real = base - deducted - frozen` (in micro-units).
    pub fn real(&self) -> i64 {
        use std::sync::atomic::Ordering::Relaxed;
        self.base_micro.load(Relaxed)
            - self.deducted_micro.load(Relaxed)
            - self.frozen_micro.load(Relaxed)
    }
}

pub static DEDUCT: Lazy<DashMap<UserId, Arc<DeductionState>>> = Lazy::new(DashMap::new);

#[derive(Clone, Copy, Debug)]
pub enum UserCircuit {
    Closed,
    Open {
        until: Instant,
        reason: CircuitReason,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum CircuitReason {
    BalanceZero,
    BannedUser,
}

pub static USER_CIRCUIT: Lazy<DashMap<UserId, UserCircuit>> = Lazy::new(DashMap::new);
