-- Postgres only shows a transaction the products of the tenant named by the
-- `soat.tenant` setting (set by `Tenant::begin`). FORCE applies the policy to
-- the table owner too, which is the role the app connects as.
ALTER TABLE products ENABLE ROW LEVEL SECURITY;
ALTER TABLE products FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON products
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
