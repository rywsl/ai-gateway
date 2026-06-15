use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fmt;

const DEFAULT_TIMEOUT_SECS: u64 = 60;

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestMeta {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    #[serde(
        default = "default_timeout",
        deserialize_with = "deserialize_timeout_secs"
    )]
    pub timeout: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseMeta {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

fn deserialize_timeout_secs<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(TimeoutVisitor)
}

struct TimeoutVisitor;

impl Visitor<'_> for TimeoutVisitor {
    type Value = u64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a non-negative integer timeout in seconds")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value)
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        u64::try_from(value).map_err(|_| E::custom("timeout must be non-negative"))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
            return Err(E::custom("timeout must be a non-negative integer"));
        }
        if value > u64::MAX as f64 {
            return Err(E::custom("timeout is too large"));
        }
        Ok(value as u64)
    }
}
