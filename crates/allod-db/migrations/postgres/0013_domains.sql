-- Domains, DNS providers, gateways and Lightning addresses as entities (SPEC §12.1, §12.7).
-- API tokens and keys live in {data_dir}/secrets, never here.
CREATE TABLE dns_providers (
    id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind       TEXT NOT NULL,              -- njalla
    label      TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    checked_at BIGINT,
    error      TEXT
);

CREATE TABLE gateways (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    label             TEXT NOT NULL DEFAULT '',
    provider          TEXT NOT NULL,              -- manual | lnvps
    ipv4              TEXT,
    ipv6              TEXT,
    mode              TEXT NOT NULL DEFAULT 'dedicated',  -- dedicated | shared
    tunnel_index      INTEGER NOT NULL UNIQUE,
    state             TEXT NOT NULL DEFAULT 'setup',
    provider_ref      TEXT,                       -- LNVPS: VM id
    provider_data     TEXT,                       -- JSON progress (LNVPS order, payment)
    expires_at        BIGINT,
    auto_renew        BOOLEAN NOT NULL DEFAULT FALSE,
    renew_max_sats    BIGINT,
    last_handshake_at BIGINT,
    checked_at        BIGINT,
    error             TEXT,
    created_at        BIGINT NOT NULL
);

CREATE TABLE domains (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name             TEXT NOT NULL UNIQUE,
    env              BOOLEAN NOT NULL DEFAULT FALSE,
    dns_provider_id  BIGINT REFERENCES dns_providers (id),
    gateway_id       BIGINT REFERENCES gateways (id),
    direct           BOOLEAN NOT NULL DEFAULT FALSE,
    managed          TEXT NOT NULL DEFAULT '{}',  -- records Allod created at the provider
    synced_at        BIGINT,
    error            TEXT,
    reach_state      TEXT,
    reach_checked_at BIGINT,
    reach_detail     TEXT,
    created_at       BIGINT NOT NULL,
    changed_at       BIGINT NOT NULL
);

CREATE TABLE ln_addresses (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    domain_id   BIGINT NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    username    TEXT NOT NULL,
    app_id      BIGINT REFERENCES apps (id),     -- NULL: Main
    offer_id    TEXT,
    verified    BOOLEAN NOT NULL DEFAULT FALSE,
    verified_at BIGINT,
    created_at  BIGINT NOT NULL,
    UNIQUE (domain_id, username)
);
CREATE INDEX ln_addresses_app ON ln_addresses (app_id);
