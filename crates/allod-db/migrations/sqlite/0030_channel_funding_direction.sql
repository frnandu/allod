-- What a splice did: 'in' or 'out' when Allod started it (the peer can splice too), and
-- the channel's capacity while each funding transaction was current, so the change shows.
ALTER TABLE channel_fundings ADD COLUMN direction TEXT;
ALTER TABLE channel_fundings ADD COLUMN capacity_sat BIGINT;
