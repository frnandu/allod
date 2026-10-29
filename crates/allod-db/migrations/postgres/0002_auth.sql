CREATE TABLE owners (
    pubkey     TEXT PRIMARY KEY NOT NULL,
    label      TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL
);

CREATE TABLE api_tokens (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name         TEXT NOT NULL,
    permission   TEXT NOT NULL CHECK (permission IN ('full', 'readonly')),
    token_hash   TEXT NOT NULL UNIQUE,
    expires_at   BIGINT,
    last_used_at BIGINT,
    created_at   BIGINT NOT NULL
);

CREATE TABLE audit_log (
    id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind       TEXT NOT NULL,
    app_id     BIGINT,
    data       TEXT NOT NULL DEFAULT '{}',
    created_at BIGINT NOT NULL
);
CREATE INDEX audit_log_kind ON audit_log (kind, id);
