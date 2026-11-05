-- Push endpoints registered by NWC connections (SPEC §8.9, NWC-XX register_push): a
-- subscription belongs to an owner or to one connection, never both. Connection pushes
-- carry their payload (the NWC-02 notification plus its event id) on the outbox row.
ALTER TABLE push_subscriptions ALTER COLUMN owner_pubkey DROP NOT NULL;
ALTER TABLE push_subscriptions ADD COLUMN app_id BIGINT REFERENCES apps (id) ON DELETE CASCADE;
ALTER TABLE push_subscriptions ADD CONSTRAINT push_subscriptions_one_owner
    CHECK ((owner_pubkey IS NULL) <> (app_id IS NULL));
CREATE INDEX push_subscriptions_app ON push_subscriptions (app_id);
ALTER TABLE push_outbox ADD COLUMN payload TEXT;
