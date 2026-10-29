-- NWC request/response log and notification outbox (SPEC §6.2, §8.2, §8.5).
-- Request content is never stored, only the method.
CREATE TABLE request_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    nostr_id   TEXT NOT NULL UNIQUE,
    app_id     INTEGER REFERENCES apps (id) ON DELETE SET NULL,
    method     TEXT,
    state      TEXT NOT NULL,   -- received | executing | executed | error
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
CREATE INDEX request_events_app ON request_events (app_id, id);

CREATE TABLE response_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    nostr_id   TEXT NOT NULL,
    request_id INTEGER NOT NULL REFERENCES request_events (id) ON DELETE CASCADE,
    state      TEXT NOT NULL,   -- sent | failed
    replied_at BIGINT,
    created_at BIGINT NOT NULL
);

CREATE TABLE notifications_outbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    app_id          INTEGER NOT NULL REFERENCES apps (id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,
    transaction_id  INTEGER NOT NULL REFERENCES transactions (id),
    attempts        INTEGER NOT NULL DEFAULT 0,
    state           TEXT NOT NULL DEFAULT 'pending',   -- pending | sent | dead
    next_attempt_at BIGINT,
    created_at      BIGINT NOT NULL,
    UNIQUE (app_id, kind, transaction_id)
);
CREATE INDEX notifications_outbox_due ON notifications_outbox (state, next_attempt_at);
