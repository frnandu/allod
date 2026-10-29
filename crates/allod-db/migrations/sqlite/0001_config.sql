-- Key/value settings. Encrypted values carry a "v1:" prefix (SPEC §4.3).
CREATE TABLE config (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    encrypted  BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
