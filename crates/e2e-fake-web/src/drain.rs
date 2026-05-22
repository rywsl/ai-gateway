//! Mock billing drain — reads `usage:records` and decrements the
//! per-user balance counter by `(input_tokens + output_tokens)` micro
//! units (1µ per token).
//!
//! In production the Go billing service consumes `usage:records` and
//! does the real cost computation; for black-box E2E we only need
//! something that moves `counter:user:{id}:balance_micro` in response
//! to recorded usage so the gateway's balance circuit can be exercised.

use crate::store::Store;
use fred::interfaces::{KeysInterface, StreamsInterface};
use std::collections::HashMap;

const USAGE_STREAM: &str = "usage:records";

/// Minimal local mirror of the gateway's UsageRecord. `serde_json` ignores
/// unknown fields by default, so we only declare what the drain needs.
#[derive(serde::Deserialize)]
struct DrainRecord {
    user_id: String,
    usage: DrainUsage,
}

#[derive(serde::Deserialize)]
struct DrainUsage {
    input_tokens: u32,
    output_tokens: u32,
}

/// Drain one batch of `usage:records` entries beyond `cursor`.
///
/// Uses `XRANGE usage:records (cursor +` (exclusive lower bound) so that
/// repeated calls advance forward without re-processing the cursor entry.
/// A malformed `payload` is logged + skipped (NOT fatal) — this is what
/// the later DLQ scenario relies on.
///
/// Returns the number of entries successfully processed.
pub async fn drain_once(store: &Store, cursor: &mut String) -> anyhow::Result<usize> {
    let client = store.redis.next();
    // '(' prefix = exclusive lower bound per XRANGE, so we never re-read the cursor entry.
    let start = format!("({cursor}");
    let entries: Vec<(String, HashMap<String, String>)> =
        client.xrange(USAGE_STREAM, start, "+", Some(1000)).await?;
    let mut processed = 0usize;
    for (entry_id, fields) in entries {
        let payload = match fields.get("payload") {
            Some(p) => p,
            None => {
                tracing::warn!(
                    target: "e2e_fake_web::drain",
                    entry_id = %entry_id,
                    "usage:records entry missing 'payload' field; skipping"
                );
                *cursor = entry_id;
                continue;
            }
        };
        let rec: DrainRecord = match serde_json::from_str(payload) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    target: "e2e_fake_web::drain",
                    entry_id = %entry_id,
                    error = %e,
                    "usage:records entry payload failed to parse; skipping"
                );
                *cursor = entry_id;
                continue;
            }
        };
        let cost = i64::from(rec.usage.input_tokens) + i64::from(rec.usage.output_tokens);
        let key = format!("counter:user:{}:balance_micro", rec.user_id);
        // INCRBY is not idempotent: on a Redis transport error we retry this entry (cursor not yet advanced), so a lost-response could double-decrement. Fault-injection scenarios must not assert exact balances.
        let _: i64 = client.incr_by(&key, -cost).await?;
        processed += 1;
        *cursor = entry_id;
    }
    Ok(processed)
}
