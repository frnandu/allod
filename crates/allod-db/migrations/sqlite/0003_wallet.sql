-- Apps (NWC connections and accounts), transactions, offers and the double-entry
-- ledger (SPEC §6.2-6.3). Timestamps are unix seconds.
CREATE TABLE apps (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    name               TEXT NOT NULL,
    description        TEXT NOT NULL DEFAULT '',
    kind               TEXT NOT NULL CHECK (kind IN ('connection', 'account')),
    app_pubkey         TEXT NOT NULL UNIQUE,
    wallet_pubkey      TEXT UNIQUE,
    isolated           BOOLEAN NOT NULL DEFAULT FALSE,
    max_amount_msat    BIGINT NOT NULL DEFAULT 0,
    budget_renewal     TEXT NOT NULL DEFAULT 'never',
    expires_at         BIGINT,
    created_by_app_id  INTEGER REFERENCES apps (id),
    lud16_username     TEXT UNIQUE,
    metadata           TEXT NOT NULL DEFAULT '{}',
    relay_urls         TEXT,
    created_at         BIGINT NOT NULL,
    updated_at         BIGINT NOT NULL,
    last_used_at       BIGINT,
    last_settled_tx_at BIGINT
);

CREATE TABLE app_permissions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    app_id     INTEGER NOT NULL REFERENCES apps (id) ON DELETE CASCADE,
    scope      TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE (app_id, scope)
);

CREATE TABLE transactions (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    public_id             TEXT NOT NULL UNIQUE,
    app_id                INTEGER REFERENCES apps (id) ON DELETE SET NULL,
    request_event_id      INTEGER,
    direction             TEXT NOT NULL CHECK (direction IN ('incoming', 'outgoing')),
    payment_type          TEXT NOT NULL,
    state                 TEXT NOT NULL,
    amount_msat           BIGINT NOT NULL,
    fee_msat              BIGINT NOT NULL DEFAULT 0,
    fee_reserve_msat      BIGINT NOT NULL DEFAULT 0,
    max_fee_msat          BIGINT,
    ln_payment_id         TEXT,
    payment_request       TEXT,
    payment_hash          TEXT,
    preimage              TEXT,
    offer_id              TEXT,
    offer                 TEXT,
    payer_note            TEXT,
    quantity              BIGINT,
    payer_proof           TEXT,
    description           TEXT NOT NULL DEFAULT '',
    description_hash      TEXT,
    metadata              TEXT,
    self_payment          BOOLEAN NOT NULL DEFAULT FALSE,
    hold                  BOOLEAN NOT NULL DEFAULT FALSE,
    settle_deadline       BIGINT,
    claimable_amount_msat BIGINT,
    counterpart_tx_id     INTEGER REFERENCES transactions (id),
    failure_reason        TEXT,
    labels                TEXT NOT NULL DEFAULT '[]',
    created_at            BIGINT NOT NULL,
    updated_at            BIGINT NOT NULL,
    expires_at            BIGINT,
    settled_at            BIGINT
);
CREATE INDEX tx_app_created ON transactions (app_id, created_at DESC);
CREATE INDEX tx_hash ON transactions (payment_hash);
CREATE INDEX tx_ln_id ON transactions (ln_payment_id);
CREATE INDEX tx_offer ON transactions (offer_id);
CREATE INDEX tx_state ON transactions (state);
CREATE UNIQUE INDEX tx_out_invoice_live ON transactions (payment_request)
    WHERE direction = 'outgoing' AND payment_type = 'bolt11' AND state IN ('PENDING', 'SETTLED');

CREATE TABLE offers (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    offer_id    TEXT NOT NULL UNIQUE,
    offer       TEXT NOT NULL,
    app_id      INTEGER REFERENCES apps (id) ON DELETE SET NULL,
    amount_msat BIGINT,
    description TEXT,
    issuer      TEXT,
    single_use  BOOLEAN NOT NULL DEFAULT FALSE,
    metadata    TEXT,
    state       TEXT NOT NULL DEFAULT 'active',
    expires_at  BIGINT,
    created_at  BIGINT NOT NULL
);

CREATE TABLE ledger_accounts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    code       TEXT NOT NULL UNIQUE,
    kind       TEXT NOT NULL,
    app_id     INTEGER UNIQUE REFERENCES apps (id),
    created_at BIGINT NOT NULL
);

CREATE TABLE journal_entries (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    kind              TEXT NOT NULL,
    transaction_id    INTEGER REFERENCES transactions (id),
    reverses_entry_id INTEGER REFERENCES journal_entries (id),
    memo              TEXT NOT NULL DEFAULT '',
    created_at        BIGINT NOT NULL,
    UNIQUE (transaction_id, kind)
);

CREATE TABLE postings (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id    INTEGER NOT NULL REFERENCES journal_entries (id),
    account_id  INTEGER NOT NULL REFERENCES ledger_accounts (id),
    amount_msat BIGINT NOT NULL,
    created_at  BIGINT NOT NULL
);
CREATE INDEX postings_account ON postings (account_id, id);

CREATE TABLE ledger_balances (
    account_id      INTEGER PRIMARY KEY REFERENCES ledger_accounts (id),
    balance_msat    BIGINT NOT NULL,
    last_posting_id INTEGER NOT NULL
);
