-- Units a buyer sends back, and what happened to them.
CREATE TABLE returns (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    order_id TEXT NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'received', 'refunded', 'rejected')),
    reason TEXT NOT NULL DEFAULT '',
    -- Who asked: a customer (cus_...) or staff (user_..., key_...).
    requested_by TEXT,
    refunded_minor BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    received_at TIMESTAMPTZ,
    closed_at TIMESTAMPTZ
);
CREATE INDEX returns_order ON returns (order_id);

CREATE TABLE return_lines (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    return_id TEXT NOT NULL REFERENCES returns (id) ON DELETE CASCADE,
    order_line_id TEXT NOT NULL REFERENCES order_lines (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    -- Where received units went back into stock; null when not restocked.
    location_id TEXT,
    PRIMARY KEY (return_id, order_line_id)
);

ALTER TABLE returns ENABLE ROW LEVEL SECURITY;
ALTER TABLE returns FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON returns
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE return_lines ENABLE ROW LEVEL SECURITY;
ALTER TABLE return_lines FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON return_lines
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
