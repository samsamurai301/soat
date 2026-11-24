-- G6: a shop's Shopify store, synced by its Admin API until cutover. The
-- access token is sealed with the install's secret key, bound to the shop.
CREATE TABLE shopify_imports (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    shop_domain TEXT NOT NULL,
    token BYTEA NOT NULL,
    products_synced_at TIMESTAMPTZ,
    customers_synced_at TIMESTAMPTZ,
    cut_over_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE shopify_imports ENABLE ROW LEVEL SECURITY;
ALTER TABLE shopify_imports FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON shopify_imports
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
