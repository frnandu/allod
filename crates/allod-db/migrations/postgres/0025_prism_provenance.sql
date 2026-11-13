ALTER TABLE transactions ADD COLUMN ln_address_id BIGINT;
CREATE INDEX tx_ln_address ON transactions (ln_address_id);

-- Old PAYING rows have no authenticated outgoing transaction link. Hold them for
-- owner review; automatic retry could pay twice if the old send succeeded.
UPDATE prism_payouts SET state = 'FAILED', next_retry_at = NULL, attempts = 10000,
    last_error = 'Payout started before provenance upgrade; review before retry'
WHERE state = 'PAYING' AND out_tx_id IS NULL;
