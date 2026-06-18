-- 0013_pricings_extended.down.sql
ALTER TABLE pricings
    DROP COLUMN IF EXISTS reasoning_micros_per_token,
    DROP COLUMN IF EXISTS prediction_micros_per_token,
    DROP COLUMN IF EXISTS web_search_micros_per_call,
    DROP COLUMN IF EXISTS cache_read_micros_per_token,
    DROP COLUMN IF EXISTS cache_creation_1h_micros_per_token,
    DROP COLUMN IF EXISTS cache_creation_5m_micros_per_token,
    DROP COLUMN IF EXISTS audio_output_micros_per_token,
    DROP COLUMN IF EXISTS audio_input_micros_per_token;
