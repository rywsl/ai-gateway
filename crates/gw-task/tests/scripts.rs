use gw_task::scripts::{RECOVER_LEASE_LUA, SETTLE_LUA, SUBMIT_LUA};

#[test]
fn test_submit_lua_creates_hash_and_xadd() {
    assert!(!SUBMIT_LUA.trim().is_empty());
    assert!(SUBMIT_LUA.contains("HSET"));
    assert!(SUBMIT_LUA.contains("XADD"));
    assert!(SUBMIT_LUA.contains("ZADD"));
    assert!(SUBMIT_LUA.contains("KEYS[1]"));
    assert!(SUBMIT_LUA.contains("KEYS[2]"));
    assert!(SUBMIT_LUA.contains("KEYS[3]"));
    assert!(SUBMIT_LUA.contains("ARGV["));
}

#[test]
fn test_settle_lua_atomic_state_transition() {
    assert!(!SETTLE_LUA.trim().is_empty());
    assert!(SETTLE_LUA.contains("HSET"));
    assert!(SETTLE_LUA.contains("ZREM"));
    assert!(SETTLE_LUA.contains("KEYS[1]"));
    assert!(SETTLE_LUA.contains("KEYS[2]"));
    assert!(SETTLE_LUA.contains("ARGV["));
}

#[test]
fn test_recover_lease_lua_reclaims_expired() {
    assert!(!RECOVER_LEASE_LUA.trim().is_empty());
    assert!(RECOVER_LEASE_LUA.contains("ZRANGEBYSCORE"));
    assert!(RECOVER_LEASE_LUA.contains("XADD"));
    assert!(RECOVER_LEASE_LUA.contains("KEYS[1]"));
    assert!(RECOVER_LEASE_LUA.contains("KEYS[2]"));
    assert!(RECOVER_LEASE_LUA.contains("ARGV["));
}
