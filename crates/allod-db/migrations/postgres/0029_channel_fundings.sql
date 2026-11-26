-- Every funding transaction a channel has had: the one that opened it, then one per
-- splice (Node → Channels links each to a block explorer). ldk-server only reports the
-- current one, so Allod keeps the history.
CREATE TABLE channel_fundings (
    user_channel_id TEXT NOT NULL,
    txid            TEXT NOT NULL,
    -- 'open' or 'splice'
    kind            TEXT NOT NULL,
    seen_at         BIGINT NOT NULL,
    PRIMARY KEY (user_channel_id, txid)
);
