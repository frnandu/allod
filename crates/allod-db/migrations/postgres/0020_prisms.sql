-- Prism splits: an address forwards a percentage of every payment it receives to other
-- Lightning targets (LNURL, BIP-353 name or BOLT12 offer). The rest stays in the
-- address's wallet. Payouts keep their own state, so a failed split is retried later and
-- the amounts that never arrived stay visible.
CREATE TABLE prism_splits (
    id          BIGSERIAL PRIMARY KEY,
    address_id  BIGINT NOT NULL REFERENCES ln_addresses (id) ON DELETE CASCADE,
    position    BIGINT NOT NULL,            -- order shown in the UI
    destination TEXT NOT NULL,
    kind        TEXT NOT NULL,              -- lnurl | bip353 | bolt12
    percent_bps INTEGER NOT NULL,           -- 1..10000 (0.01% .. 100%)
    created_at  BIGINT NOT NULL,
    UNIQUE (address_id, position)
);

CREATE TABLE prism_payouts (
    id             BIGSERIAL PRIMARY KEY,
    address_id     BIGINT NOT NULL REFERENCES ln_addresses (id) ON DELETE CASCADE,
    incoming_tx_id BIGINT NOT NULL,         -- transactions.id of the received payment
    destination    TEXT NOT NULL,
    kind           TEXT NOT NULL,
    amount_msat    BIGINT NOT NULL,         -- this split's share of the payment
    fee_msat       BIGINT NOT NULL DEFAULT 0,
    percent_bps    INTEGER NOT NULL,        -- as configured when the payment was split
    -- PENDING | PAYING | PAID | FAILED | CANCELED | SKIPPED
    state          TEXT NOT NULL,
    attempts       INTEGER NOT NULL DEFAULT 0,
    last_error     TEXT,
    payment_hash   TEXT,
    out_tx_id      BIGINT,
    next_retry_at  BIGINT,
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL,
    paid_at        BIGINT,
    UNIQUE (incoming_tx_id, destination)
);
CREATE INDEX prism_payouts_due ON prism_payouts (state, next_retry_at);
CREATE INDEX prism_payouts_address ON prism_payouts (address_id, created_at);
