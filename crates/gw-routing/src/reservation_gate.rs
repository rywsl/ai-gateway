use crate::reservation::ReservationGlobalView;
use gw_core::ids::ModelName;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct ReservationGate {
    enabled: bool,
    thresholds: Vec<(String, u64)>,
}

impl ReservationGate {
    pub fn disabled(thresholds: HashMap<String, u64>) -> Self {
        Self::new(false, thresholds)
    }

    pub fn enabled(thresholds: HashMap<String, u64>) -> Self {
        Self::new(true, thresholds)
    }

    pub fn new(enabled: bool, thresholds: HashMap<String, u64>) -> Self {
        Self {
            enabled,
            thresholds: thresholds.into_iter().collect(),
        }
    }

    pub fn allows(&self, model: &ModelName, view: Option<&ReservationGlobalView>) -> bool {
        if !self.enabled || self.thresholds.is_empty() {
            return true;
        }
        let Some(threshold_rpm) = self.threshold_for(model.as_ref()) else {
            return true;
        };
        let Some(global_view) = view.and_then(|view| view.get(model)) else {
            return true;
        };
        let threshold_per_sec = threshold_rpm as f64 / 60.0;
        global_view.total_rate <= threshold_per_sec
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
