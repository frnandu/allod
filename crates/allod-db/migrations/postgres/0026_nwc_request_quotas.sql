ALTER TABLE request_events ADD COLUMN storage_bytes BIGINT NOT NULL DEFAULT 128;
ALTER TABLE response_events ADD COLUMN storage_bytes BIGINT NOT NULL DEFAULT 96;

CREATE TABLE nwc_request_usage (
    app_id      BIGINT PRIMARY KEY REFERENCES apps (id) ON DELETE CASCADE,
    row_count   BIGINT NOT NULL DEFAULT 0,
    byte_count  BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE nwc_request_usage_global (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    row_count   BIGINT NOT NULL DEFAULT 0,
    byte_count  BIGINT NOT NULL DEFAULT 0
);

INSERT INTO nwc_request_usage (app_id, row_count, byte_count)
SELECT app_id, SUM(rows), SUM(bytes) FROM (
    SELECT app_id, COUNT(*) AS rows, COALESCE(SUM(storage_bytes), 0) AS bytes
      FROM request_events WHERE app_id IS NOT NULL GROUP BY app_id
    UNION ALL
    SELECT r.app_id, COUNT(*) AS rows, COALESCE(SUM(e.storage_bytes), 0) AS bytes
      FROM response_events e JOIN request_events r ON r.id = e.request_id
      WHERE r.app_id IS NOT NULL GROUP BY r.app_id
) usage GROUP BY app_id;

INSERT INTO nwc_request_usage_global (id, row_count, byte_count)
SELECT 1,
       (SELECT COUNT(*) FROM request_events) + (SELECT COUNT(*) FROM response_events),
       COALESCE((SELECT SUM(storage_bytes) FROM request_events), 0) +
       COALESCE((SELECT SUM(storage_bytes) FROM response_events), 0);
