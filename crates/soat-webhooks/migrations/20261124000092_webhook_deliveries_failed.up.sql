-- Given-up deliveries, counted by /metrics without scanning the table.
CREATE INDEX webhook_deliveries_failed ON webhook_deliveries (failed_at)
    WHERE failed_at IS NOT NULL;
