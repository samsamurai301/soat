-- Given-up deliveries, counted by /metrics without scanning the table.
CREATE INDEX event_deliveries_failed ON event_deliveries (failed_at)
    WHERE failed_at IS NOT NULL;
