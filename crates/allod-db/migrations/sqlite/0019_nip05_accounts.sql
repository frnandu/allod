ALTER TABLE nip05_names ADD COLUMN account_id INTEGER REFERENCES apps(id) ON DELETE SET NULL;
CREATE UNIQUE INDEX nip05_names_account_id_unique ON nip05_names(account_id) WHERE account_id IS NOT NULL;
