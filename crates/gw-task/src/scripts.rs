pub const SUBMIT_LUA: &str = r#"
local task_id = ARGV[1]
local lease_until_ms = ARGV[2]
local lease_seconds = ARGV[3]
local task_type = ARGV[4]
local vendor_id = ARGV[5]
local ttl_seconds = tonumber(ARGV[6])

if ((#ARGV - 6) % 2) ~= 0 then
    return redis.error_reply('task hash fields must be field/value pairs')
end

for i = 7, #ARGV, 2 do
    redis.call('HSET', KEYS[1], ARGV[i], ARGV[i + 1])
end

if ttl_seconds and ttl_seconds > 0 then
    redis.call('EXPIRE', KEYS[1], ttl_seconds)
end

redis.call(
    'XADD', KEYS[2], '*',
    'task_id', task_id,
    'task_type', task_type,
    'vendor_id', vendor_id,
    'lease_seconds', lease_seconds
)
redis.call('ZADD', KEYS[3], lease_until_ms, task_id)

return 'ok'
"#;

pub const TASK_FREEZE_LUA: &str = r#"
local task_id = ARGV[1]
local amount = tonumber(ARGV[2])
local available = ARGV[3]

if not amount or amount < 0 then
    return redis.error_reply('invalid task freeze amount')
end

local already = tonumber(redis.call('GET', KEYS[1]) or '0')
if available ~= '' then
    local available_num = tonumber(available)
    if not available_num then
        return redis.error_reply('invalid available balance')
    end
    if (available_num - already) < amount then
        return 'insufficient'
    end
end

redis.call('INCRBY', KEYS[1], amount)
redis.call('SADD', KEYS[2], task_id)

return 'ok'
"#;

pub const SETTLE_LUA: &str = r#"
local task_id = ARGV[1]
local terminal_status = ARGV[2]
local settled_at_ms = ARGV[3]
local released_micro = tonumber(ARGV[4]) or 0
local deducted_micro = tonumber(ARGV[5]) or 0
local usage_payload = ARGV[6]

local cur = redis.call('HGET', KEYS[1], 'status')
if cur == 'settled' or cur == 'cancelled' or cur == 'expired' then
    return 'noop'
end

if ((#ARGV - 6) % 2) ~= 0 then
    return redis.error_reply('settle fields must be field/value pairs')
end

redis.call(
    'HSET', KEYS[1],
    'status', terminal_status,
    'settled_at_ms', settled_at_ms
)

for i = 7, #ARGV, 2 do
    redis.call('HSET', KEYS[1], ARGV[i], ARGV[i + 1])
end

redis.call('ZREM', KEYS[2], task_id)
redis.call('INCRBY', KEYS[3], -released_micro)
redis.call('INCRBY', KEYS[4], deducted_micro)
redis.call('SREM', KEYS[6], task_id)
redis.call('XADD', KEYS[5], '*', 'payload', usage_payload)

return 'ok'
"#;

pub const RECOVER_LEASE_LUA: &str = r#"
local now_ms = ARGV[1]
local limit = tonumber(ARGV[2])
local lease_seconds = ARGV[3]

if not limit or limit <= 0 then
    return {}
end

local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now_ms, 'LIMIT', 0, limit)

for _, task_id in ipairs(expired) do
    local removed = redis.call('ZREM', KEYS[1], task_id)
    if removed == 1 then
        redis.call(
            'XADD', KEYS[2], '*',
            'task_id', task_id,
            'recovered', '1',
            'lease_seconds', lease_seconds
        )
    end
end

return expired
"#;
