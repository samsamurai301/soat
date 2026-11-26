-- Hand-picked groups of products, in the merchant's order.
CREATE TABLE collections (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    handle TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    UNIQUE (tenant_id, handle)
);

CREATE TABLE collection_products (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    collection_id TEXT NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    position BIGINT NOT NULL,
    PRIMARY KEY (collection_id, product_id)
);
CREATE INDEX collection_products_order ON collection_products (collection_id, position);
CREATE INDEX collection_products_product ON collection_products (product_id);

ALTER TABLE collections ENABLE ROW LEVEL SECURITY;
ALTER TABLE collections FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON collections
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE collection_products ENABLE ROW LEVEL SECURITY;
ALTER TABLE collection_products FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON collection_products
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
