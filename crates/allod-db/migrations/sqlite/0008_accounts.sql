-- Accounts (SPEC §10): closing an account keeps its row, so its ledger account
-- (`wallet:app:<id>`), statement and transaction history stay attached to it and never
-- become visible as Main history. A closed account answers no NWC requests.
ALTER TABLE apps ADD COLUMN closed_at BIGINT;
