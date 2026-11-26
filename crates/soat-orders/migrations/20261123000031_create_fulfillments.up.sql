-- Shipments of an order's lines.
CREATE TABLE fulfillments (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    order_id TEXT NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'shipped' CHECK (status IN ('shipped', 'delivered', 'canceled')),
    carrier TEXT NOT NULL DEFAULT '',
    tracking_number TEXT NOT NULL DEFAULT '',
    tracking_url TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ,
    canceled_at TIMESTAMPTZ
);
CREATE INDEX fulfillments_order ON fulfillments (order_id);

CREATE TABLE fulfillment_lines (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    fulfillment_id TEXT NOT NULL REFERENCES fulfillments (id) ON DELETE CASCADE,
    order_line_id TEXT NOT NULL REFERENCES order_lines (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    -- Where the units left from; null for untracked items.
    location_id TEXT,
    PRIMARY KEY (fulfillment_id, order_line_id)
);

ALTER TABLE fulfillments ENABLE ROW LEVEL SECURITY;
ALTER TABLE fulfillments FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON fulfillments
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE fulfillment_lines ENABLE ROW LEVEL SECURITY;
ALTER TABLE fulfillment_lines FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON fulfillment_lines
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
