-- Push endpoints registered by NWC connections (SPEC §8.9, NWC-XX register_push): a
-- subscription belongs to an owner or to one connection, never both. Connection pushes
-- carry their payload (the NWC-02 notification plus its event id) on the outbox row.
-- SQLite cannot relax NOT NULL in place, so both tables are rebuilt. The outbox is
-- dropped first, so dropping the subscriptions cascades into nothing.
CREATE TABLE push_subscriptions_v2 (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_pubkey    TEXT REFERENCES owners (pubkey) ON DELETE CASCADE,
    app_id          INTEGER REFERENCES apps (id) ON DELETE CASCADE,
    endpoint        TEXT NOT NULL UNIQUE,
    p256dh          TEXT NOT NULL,
    auth            TEXT NOT NULL,
    label           TEXT NOT NULL DEFAULT '',
    events          TEXT NOT NULL,
    hide_amounts    BOOLEAN NOT NULL DEFAULT FALSE,
    failures        INTEGER NOT NULL DEFAULT 0,
    last_error      TEXT,
    last_success_at BIGINT,
    created_at      BIGINT NOT NULL,
    updated_at      BIGINT NOT NULL,
    CHECK ((owner_pubkey IS NULL) <> (app_id IS NULL))
);
INSERT INTO push_subscriptions_v2 (id, owner_pubkey, endpoint, p256dh, auth, label, events,
    hide_amounts, failures, last_error, last_success_at, created_at, updated_at)
SELECT id, owner_pubkey, endpoint, p256dh, auth, label, events, hide_amounts, failures,
    last_error, last_success_at, created_at, updated_at FROM push_subscriptions;

CREATE TABLE push_outbox_v2 (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    subscription_id INTEGER NOT NULL REFERENCES push_subscriptions_v2 (id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,
    transaction_id  INTEGER NOT NULL REFERENCES transactions (id),
    payload         TEXT,
    attempts        INTEGER NOT NULL DEFAULT 0,
    state           TEXT NOT NULL DEFAULT 'pending',
    next_attempt_at BIGINT,
    created_at      BIGINT NOT NULL,
    UNIQUE (subscription_id, kind, transaction_id)
);
INSERT INTO push_outbox_v2 (id, subscription_id, kind, transaction_id, attempts, state,
    next_attempt_at, created_at)
SELECT id, subscription_id, kind, transaction_id, attempts, state, next_attempt_at,
    created_at FROM push_outbox;

DROP TABLE push_outbox;
DROP TABLE push_subscriptions;
ALTER TABLE push_subscriptions_v2 RENAME TO push_subscriptions;
ALTER TABLE push_outbox_v2 RENAME TO push_outbox;
CREATE INDEX push_outbox_due ON push_outbox (state, next_attempt_at);
CREATE INDEX push_subscriptions_app ON push_subscriptions (app_id);
