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
    assert!(
        SETTLE_LUA.contains("INCRBY")
            && SETTLE_LUA.contains("KEYS[3]")
            && SETTLE_LUA.contains("KEYS[4]"),
        "settle must release task freeze and deduct actual balance via scalar counters"
    );
    assert!(
        SETTLE_LUA.contains("XADD") && SETTLE_LUA.contains("KEYS[5]"),
        "settle must emit a usage record atomically"
    );
    assert!(SETTLE_LUA.contains("ARGV["));
}

#[test]
fn test_settle_lua_terminal_status_guard_precedes_mutations() {
    let guard = "local cur = redis.call('HGET', KEYS[1], 'status')";
    let noop = "return 'noop'";

    let guard_pos = SETTLE_LUA
        .find(guard)
        .expect("settle lua must read existing task status before mutating");
    let noop_pos = SETTLE_LUA
        .find(noop)
        .expect("settle lua must return noop for already-terminal tasks");
    let hset_pos = SETTLE_LUA
        .find("'HSET', KEYS[1]")
        .expect("settle lua must keep task hash mutation");
    let incrby_pos = SETTLE_LUA
        .find("'INCRBY'")
        .expect("settle lua must keep counter mutations");
    let xadd_pos = SETTLE_LUA
        .find("'XADD'")
        .expect("settle lua must keep usage record emission");

    assert!(SETTLE_LUA.contains("cur == 'settled'"));
    assert!(SETTLE_LUA.contains("cur == 'cancelled'"));
    assert!(SETTLE_LUA.contains("cur == 'expired'"));
    assert!(guard_pos < noop_pos);
    assert!(noop_pos < hset_pos);
    assert!(noop_pos < incrby_pos);
    assert!(noop_pos < xadd_pos);
}

#[test]
fn test_settle_caller_accepts_noop_as_idempotent_success() {
    let contract_src = include_str!("../src/redis_contract.rs");

    assert!(
        contract_src.contains("result != \"ok\" && result != \"noop\""),
        "settle caller must accept Lua noop as idempotent terminal-race success"
    );
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
