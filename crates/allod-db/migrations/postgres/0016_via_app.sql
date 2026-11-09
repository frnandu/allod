-- The connection that asked for an invoice owned by another account: a Lightning
-- address service's receive-only connection for an account (SPEC §12.6). That
-- connection sees these rows, and only these.
ALTER TABLE transactions ADD COLUMN via_app_id BIGINT;
CREATE INDEX transactions_via_app ON transactions (via_app_id);
