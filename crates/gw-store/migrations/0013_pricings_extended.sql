-- 0013_pricings_extended.sql - REV-FINAL-H8.5 + C1: add new usage
-- dimensions to pricings. All NULL by default; settler treats NULL as
-- "no price defined" and refuses to settle instead of silently charging 0.
ALTER TABLE pricings
    ADD COLUMN IF NOT EXISTS audio_input_micros_per_token       NUMERIC(18,9) NULL,
    ADD COLUMN IF NOT EXISTS audio_output_micros_per_token      NUMERIC(18,9) NULL,
    ADD COLUMN IF NOT EXISTS cache_creation_5m_micros_per_token NUMERIC(18,9) NULL,
    ADD COLUMN IF NOT EXISTS cache_creation_1h_micros_per_token NUMERIC(18,9) NULL,
    ADD COLUMN IF NOT EXISTS cache_read_micros_per_token        NUMERIC(18,9) NULL,
    ADD COLUMN IF NOT EXISTS web_search_micros_per_call         NUMERIC(18,9) NULL,
    ADD COLUMN IF NOT EXISTS prediction_micros_per_token        NUMERIC(18,9) NULL,
    ADD COLUMN IF NOT EXISTS reasoning_micros_per_token         NUMERIC(18,9) NULL;
