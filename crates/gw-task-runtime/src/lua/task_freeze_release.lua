-- Keys: KEYS[1] = {user:<uid>}:tasks:leases
--       KEYS[2] = {user:<uid>}:entity:task:<task_id>
--       KEYS[3] = {user:<uid>}:counter:task_frozen_micro
-- Args: ARGV[1] = task_id
--       ARGV[2] = frozen_amount (string, will be negated)

local removed = redis.call('ZREM', KEYS[1], ARGV[1])
if removed == 0 then
    return 'already_released'
end
redis.call('HSET', KEYS[2], 'status', 'released')
redis.call('HINCRBY', KEYS[3], 'frozen_micro', -tonumber(ARGV[2]))
return 'ok'
