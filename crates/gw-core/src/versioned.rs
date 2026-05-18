use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Versioned<T> {
    pub version: u64,
    pub value: Arc<T>,
}

impl<T> Versioned<T> {
    pub fn new(version: u64, value: T) -> Self {
        Self {
            version,
            value: Arc::new(value),
        }
    }
}

/// 严格大于才覆盖（ADR-006）。
pub fn upsert_if_newer<T>(current: &mut Option<Versioned<T>>, candidate: Versioned<T>) -> bool {
    match current {
        Some(c) if c.version >= candidate.version => false,
        _ => {
            *current = Some(candidate);
            true
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Json,
    Msgpack,
}
