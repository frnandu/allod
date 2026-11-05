-- Device notifications over Web Push (SPEC §8.9). Subscriptions belong to an owner and
-- disappear with them; the outbox is written in the same transaction as the payment.
CREATE TABLE push_subscriptions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_pubkey    TEXT NOT NULL REFERENCES owners (pubkey) ON DELETE CASCADE,
    endpoint        TEXT NOT NULL UNIQUE,
    p256dh          TEXT NOT NULL,
    auth            TEXT NOT NULL,
    label           TEXT NOT NULL DEFAULT '',
    events          TEXT NOT NULL,   -- comma-separated notification types
    hide_amounts    BOOLEAN NOT NULL DEFAULT FALSE,
    failures        INTEGER NOT NULL DEFAULT 0,
    last_error      TEXT,
    last_success_at BIGINT,
    created_at      BIGINT NOT NULL,
    updated_at      BIGINT NOT NULL
);

CREATE TABLE push_outbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    subscription_id INTEGER NOT NULL REFERENCES push_subscriptions (id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,
    transaction_id  INTEGER NOT NULL REFERENCES transactions (id),
    attempts        INTEGER NOT NULL DEFAULT 0,
    state           TEXT NOT NULL DEFAULT 'pending',   -- pending | sent | dead
    next_attempt_at BIGINT,
    created_at      BIGINT NOT NULL,
    UNIQUE (subscription_id, kind, transaction_id)
);
CREATE INDEX push_outbox_due ON push_outbox (state, next_attempt_at);
