-- Distinguish retries of one BOLT11 payment hash from stale backend records/events.
ALTER TABLE transactions ADD COLUMN attempt_number BIGINT NOT NULL DEFAULT 1;
ALTER TABLE transactions ADD COLUMN attempt_baseline_updated_at BIGINT;
ALTER TABLE transactions ADD COLUMN attempt_submitted_at BIGINT;
