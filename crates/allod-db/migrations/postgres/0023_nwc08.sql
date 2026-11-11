-- NWC-08 (client-initiated connections). `auth_state` is the client's correlation value,
-- repeated in the connection's info event. `auth_relays` are the client's relays that
-- still have to receive that info event (cleared once it is delivered). `methods` and
-- `notification_types` narrow a connection to what the client asked for; NULL means
-- everything its permissions allow. All lists are comma-separated.
ALTER TABLE apps ADD COLUMN auth_state TEXT;
ALTER TABLE apps ADD COLUMN auth_relays TEXT;
ALTER TABLE apps ADD COLUMN methods TEXT;
ALTER TABLE apps ADD COLUMN notification_types TEXT;
