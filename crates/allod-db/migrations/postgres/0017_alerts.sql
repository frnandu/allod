-- Owner alerts (SPEC §8.9): things that need the owner's attention that are not
-- payments, such as a gateway about to expire. Shown in the app and pushed to the
-- owner's devices; `key` makes raising the same alert twice a no-op.
CREATE TABLE alerts (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key          TEXT NOT NULL UNIQUE,
    kind         TEXT NOT NULL,
    title        TEXT NOT NULL,
    body         TEXT NOT NULL,
    url          TEXT,
    created_at   BIGINT NOT NULL,
    dismissed_at BIGINT
);

CREATE TABLE alert_outbox (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    subscription_id BIGINT NOT NULL REFERENCES push_subscriptions (id) ON DELETE CASCADE,
    alert_id        BIGINT NOT NULL REFERENCES alerts (id) ON DELETE CASCADE,
    attempts        INTEGER NOT NULL DEFAULT 0,
    state           TEXT NOT NULL DEFAULT 'pending',
    next_attempt_at BIGINT,
    created_at      BIGINT NOT NULL,
    UNIQUE (subscription_id, alert_id)
);
CREATE INDEX alert_outbox_due ON alert_outbox (state, next_attempt_at);
