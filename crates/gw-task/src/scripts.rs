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

pub const SETTLE_LUA: &str = r#"
local task_id = ARGV[1]
local terminal_status = ARGV[2]
local settled_at_ms = ARGV[3]

if ((#ARGV - 3) % 2) ~= 0 then
    return redis.error_reply('settle fields must be field/value pairs')
end

redis.call(
    'HSET', KEYS[1],
    'status', terminal_status,
    'settled_at_ms', settled_at_ms
)

for i = 4, #ARGV, 2 do
    redis.call('HSET', KEYS[1], ARGV[i], ARGV[i + 1])
end

redis.call('ZREM', KEYS[2], task_id)

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
