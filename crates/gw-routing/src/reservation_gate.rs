use crate::reservation::ReservationGlobalView;
use gw_core::ids::ModelName;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const DEFAULT_STARTUP_GRACE_SECS: u64 = 30;

#[derive(Clone, Debug, Default)]
pub struct ReservationGate {
    enabled: bool,
    thresholds: Vec<(String, u64)>,
    startup_grace: Duration,
    started_at: Option<Instant>,
}

impl ReservationGate {
    pub fn disabled(thresholds: HashMap<String, u64>) -> Self {
        Self::new(false, thresholds)
    }

    pub fn enabled(thresholds: HashMap<String, u64>) -> Self {
        Self::new(true, thresholds)
    }

    pub fn new(enabled: bool, thresholds: HashMap<String, u64>) -> Self {
        Self::with_startup_grace(
            enabled,
            thresholds,
            Duration::from_secs(DEFAULT_STARTUP_GRACE_SECS),
        )
    }

    pub fn with_startup_grace(
        enabled: bool,
        thresholds: HashMap<String, u64>,
        startup_grace: Duration,
    ) -> Self {
        Self {
            enabled,
            thresholds: thresholds.into_iter().collect(),
            startup_grace,
            started_at: Some(Instant::now()),
        }
    }

    pub fn allows(&self, model: &ModelName, view: Option<&ReservationGlobalView>) -> bool {
        if !self.enabled || self.thresholds.is_empty() {
            return true;
        }
        let Some(threshold_rpm) = self.threshold_for(model.as_ref()) else {
            return true;
        };
        if !self.bootstrap_complete(view) {
            return false;
        }
        let Some(global_view) = view.and_then(|view| view.get(model)) else {
            return true;
        };
        let threshold_per_sec = threshold_rpm as f64 / 60.0;
        global_view.total_rate <= threshold_per_sec
    }

    pub fn bootstrap_complete(&self, view: Option<&ReservationGlobalView>) -> bool {
        if view.is_some_and(ReservationGlobalView::has_received_event) {
            return true;
        }
        self.started_at
            .map(|started_at| started_at.elapsed() > self.startup_grace)
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub fn with_started_at_for_test(mut self, started_at: Instant) -> Self {
        self.started_at = Some(started_at);
        self
    }

    fn threshold_for(&self, model: &str) -> Option<u64> {
        self.thresholds
            .iter()
            .filter_map(|(pattern, threshold)| {
                pattern_matches(pattern, model).then_some(*threshold)
            })
            .min()
    }
}

fn pattern_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    pattern == value
}
