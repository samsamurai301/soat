-- Labels printed for shipments through a carrier account.
CREATE TABLE shipping_labels (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    fulfillment_id TEXT NOT NULL REFERENCES fulfillments (id) ON DELETE CASCADE,
    carrier_account_id TEXT NOT NULL,
    carrier TEXT NOT NULL,
    tracking_number TEXT NOT NULL,
    tracking_url TEXT NOT NULL,
    carrier_ref TEXT NOT NULL,
    file_id TEXT REFERENCES files (id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'canceled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX shipping_labels_fulfillment ON shipping_labels (tenant_id, fulfillment_id);

ALTER TABLE shipping_labels ENABLE ROW LEVEL SECURITY;
ALTER TABLE shipping_labels FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON shipping_labels
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
