-- Products a customer saved for later.
CREATE TABLE wishlist_items (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, customer_id, product_id)
);
CREATE INDEX wishlist_items_product ON wishlist_items (tenant_id, product_id);

ALTER TABLE wishlist_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE wishlist_items FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON wishlist_items
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
