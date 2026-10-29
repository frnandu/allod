-- Owners: Nostr public keys allowed to log in to the web UI (SPEC §11.1). Not secret.
CREATE TABLE owners (
    pubkey     TEXT PRIMARY KEY NOT NULL,   -- x-only, lowercase hex
    label      TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL              -- unix seconds
);

-- API tokens for scripts and the CLI. Only the SHA-256 of the token is stored.
CREATE TABLE api_tokens (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT NOT NULL,
    permission   TEXT NOT NULL CHECK (permission IN ('full', 'readonly')),
    token_hash   TEXT NOT NULL UNIQUE,
    expires_at   BIGINT,
    last_used_at BIGINT,
    created_at   BIGINT NOT NULL
);

CREATE TABLE audit_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL,
    app_id     INTEGER,
    data       TEXT NOT NULL DEFAULT '{}',   -- JSON
    created_at BIGINT NOT NULL
);
CREATE INDEX audit_log_kind ON audit_log (kind, id);
