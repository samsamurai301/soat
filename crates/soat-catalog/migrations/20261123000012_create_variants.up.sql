-- Options (Size, Color...) of a product, in order, with their values.
CREATE TABLE product_options (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    option_values TEXT[] NOT NULL,
    PRIMARY KEY (product_id, position)
);

-- A sellable version of a product: one value per option, in option order.
-- Prices are integer minor units in the product's currency.
CREATE TABLE product_variants (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    sku TEXT NOT NULL,
    price_minor BIGINT NOT NULL CHECK (price_minor >= 0),
    option_values TEXT[] NOT NULL,
    UNIQUE (tenant_id, sku),
    UNIQUE (product_id, option_values)
);
CREATE INDEX product_variants_product ON product_variants (product_id, id);

ALTER TABLE product_options ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_options FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON product_options
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE product_variants ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_variants FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON product_variants
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
