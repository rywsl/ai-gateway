//! G3 (T2-A) — pure-function shape test for `daily_tokens_key`.
//!
//! Spec: ADR-G3-1. The helper is the single source of truth for the Redis
//! key shape used by both the pre-check in `request_filter` and the
//! post-increment in `logging`.

use gw_core::ids::UserId;
use gw_store::counter_store::daily_tokens_key;

#[test]
fn key_shape() {
    let k = daily_tokens_key(&UserId("u1".into()), "2026-05-21");
    assert_eq!(k, "counter:user:u1:daily_tokens:2026-05-21");
}

#[test]
fn key_handles_alternative_user_id() {
    let k = daily_tokens_key(&UserId("acc-7f3e".into()), "2025-12-31");
    assert_eq!(k, "counter:user:acc-7f3e:daily_tokens:2025-12-31");
}
