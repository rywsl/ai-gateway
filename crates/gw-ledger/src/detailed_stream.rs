//! Detailed log redaction + verbosity gating.
//!
//! Write-time redaction (rather than read-time) avoids the entire class of bugs
//! where a future consumer forgets to apply a filter. The denylist and
//! allowlist are `const` so the lint in `scripts/check-invariants.sh` can
//! detect any drift outside the named regions.

use serde_json::Value;

/// Headers that MUST NEVER appear in detailed logs.
pub const REDACT_HEADER_DENYLIST: &[&str] = &["authorization", "x-api-key", "cookie"];

/// Headers that MAY appear in detailed logs.
pub const REDACT_HEADER_ALLOWLIST: &[&str] = &[
    "user-agent",
    "accept-encoding",
    "content-type",
    "content-length",
    "x-request-id",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogVerbosity {
    Base,
    Headers,
    Full,
}

/// Maximum serialized size of a single detailed log record. Larger records are
/// dropped at the sink layer (with a counter increment).
pub const MAX_DETAILED_LOG_BYTES: usize = 64 * 1024;

/// Keep only headers in the allowlist (plus the `x-stainless-*` SDK telemetry
/// family). The denylist is enforced by the allowlist's closed shape — denied
/// headers can never appear because they're not allowed.
pub fn redact_headers(input: &[(String, String)]) -> Vec<(String, String)> {
    input
        .iter()
        .filter(|(k, _)| {
            let kl = k.to_ascii_lowercase();
            REDACT_HEADER_ALLOWLIST.contains(&kl.as_str()) || kl.starts_with("x-stainless-")
        })
        .cloned()
        .collect()
}

/// Walk the JSON body redacting all user content. Preserves structural fields
/// (`role`, `model`, `tool_calls.name`, etc.) so downstream analytics can
/// still categorize requests without seeing payload data.
pub fn redact_body(body: Value) -> Value {
    walk(body)
}

fn walk(v: Value) -> Value {
    match v {
        Value::Object(mut m) => {
            if let Some(Value::Array(arr)) = m.get_mut("messages") {
                for msg in arr.iter_mut() {
                    if let Value::Object(o) = msg {
                        if let Some(c) = o.get_mut("content") {
                            *c = redact_message_content(c.take());
                        }
                    }
                }
            }
            if let Some(Value::Array(arr)) = m.get_mut("input") {
                for it in arr.iter_mut() {
                    *it = redact_part(it.take());
                }
            }
            for (_k, vv) in m.iter_mut() {
                *vv = walk(vv.take());
            }
            Value::Object(m)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(walk).collect()),
        other => other,
    }
}

fn redact_message_content(c: Value) -> Value {
    match c {
        Value::String(s) => Value::String(format!("<redacted-text-{}-chars>", s.chars().count())),
        Value::Array(arr) => Value::Array(arr.into_iter().map(redact_part).collect()),
        Value::Object(_) => redact_part(c),
        other => other,
    }
}

fn redact_part(p: Value) -> Value {
    if let Value::Object(mut m) = p {
        let typ = m
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        match typ.as_str() {
            "text" | "input_text" => {
                if let Some(t) = m.get("text").and_then(|v| v.as_str()) {
                    let n = t.chars().count();
                    m.insert(
                        "text".into(),
                        Value::String(format!("<redacted-text-{n}-chars>")),
                    );
                }
            }
            "image" | "input_image" | "audio" | "input_audio" | "video" => {
                let media_type = m
                    .get("source")
                    .and_then(|s| s.get("media_type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("application/octet-stream");
                let size = m
                    .get("source")
                    .and_then(|s| s.get("data"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.len())
                    .unwrap_or(0);
                m.insert(
                    "source".into(),
                    Value::String(format!("<media:type={media_type};size={size}>")),
                );
            }
            _ => {}
        }
        return Value::Object(m);
    }
    p
}
