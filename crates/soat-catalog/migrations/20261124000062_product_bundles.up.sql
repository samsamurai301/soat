-- A bundle is a product (without variants) sold as a set of other stock
-- items: its stock comes from theirs.
CREATE TABLE product_bundle_items (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    bundle_id TEXT NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity BETWEEN 1 AND 1000),
    position INTEGER NOT NULL,
    PRIMARY KEY (bundle_id, item_id)
);
CREATE INDEX product_bundle_items_item ON product_bundle_items (tenant_id, item_id);

ALTER TABLE product_bundle_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_bundle_items FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON product_bundle_items
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
