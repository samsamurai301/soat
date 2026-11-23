-- The outbox: every domain event, written in the transaction of the change.
-- The dispatcher reads across shops by setting `soat.system` to `on`; no
-- request handler ever sets it.
CREATE TABLE events (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Deliveries have been created for every interested subscriber.
    fanned_out BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE INDEX events_pending ON events (id) WHERE NOT fanned_out;

-- One row per event and subscriber: at-least-once, retried with backoff.
CREATE TABLE event_deliveries (
    event_id TEXT NOT NULL REFERENCES events (id) ON DELETE CASCADE,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    subscriber TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ,
    -- Set after the last retry; the delivery is given up.
    failed_at TIMESTAMPTZ,
    last_error TEXT,
    PRIMARY KEY (event_id, subscriber)
);
CREATE INDEX event_deliveries_due ON event_deliveries (next_attempt_at)
    WHERE delivered_at IS NULL AND failed_at IS NULL;

ALTER TABLE events ENABLE ROW LEVEL SECURITY;
ALTER TABLE events FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON events
    USING (tenant_id = current_setting('soat.tenant', true)
           OR current_setting('soat.system', true) = 'on')
    WITH CHECK (tenant_id = current_setting('soat.tenant', true)
                OR current_setting('soat.system', true) = 'on');

ALTER TABLE event_deliveries ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_deliveries FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON event_deliveries
    USING (tenant_id = current_setting('soat.tenant', true)
           OR current_setting('soat.system', true) = 'on')
    WITH CHECK (tenant_id = current_setting('soat.tenant', true)
                OR current_setting('soat.system', true) = 'on');
