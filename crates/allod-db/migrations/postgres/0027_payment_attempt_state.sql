-- Submission provenance for BOLT11 retries sharing one backend payment hash.
ALTER TABLE transactions ADD COLUMN attempt_state TEXT NOT NULL DEFAULT 'legacy';

-- Existing pending attempts may have lost an RPC response. Never assume their
-- backend FAILED row belongs to newest generation.
UPDATE transactions SET attempt_state = CASE
    WHEN payment_type = 'bolt11' AND state = 'PENDING' THEN 'ambiguous'
    ELSE 'finished'
END;
