-- Countries a shop ships to, grouped by how it charges.
CREATE TABLE shipping_zones (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    UNIQUE (tenant_id, name)
);

-- A country belongs to at most one shipping zone of a shop.
CREATE TABLE shipping_zone_countries (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    zone_id TEXT NOT NULL REFERENCES shipping_zones (id) ON DELETE CASCADE,
    country TEXT NOT NULL,
    PRIMARY KEY (tenant_id, country)
);
CREATE INDEX shipping_zone_countries_zone ON shipping_zone_countries (zone_id);

-- A way to ship into a zone and what it costs. Bounds are inclusive and
-- optional; a rate applies when the order's value and weight fit them.
CREATE TABLE shipping_rates (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    zone_id TEXT NOT NULL REFERENCES shipping_zones (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    carrier TEXT NOT NULL DEFAULT '',
    currency TEXT NOT NULL,
    price_minor BIGINT NOT NULL CHECK (price_minor >= 0),
    min_subtotal_minor BIGINT CHECK (min_subtotal_minor >= 0),
    max_subtotal_minor BIGINT CHECK (max_subtotal_minor >= 0),
    min_weight_grams BIGINT CHECK (min_weight_grams >= 0),
    max_weight_grams BIGINT CHECK (max_weight_grams >= 0),
    min_days INTEGER CHECK (min_days >= 0),
    max_days INTEGER CHECK (max_days >= 0),
    tax_class TEXT NOT NULL DEFAULT 'standard',
    active BOOLEAN NOT NULL DEFAULT TRUE
);
CREATE INDEX shipping_rates_zone ON shipping_rates (zone_id);

ALTER TABLE shipping_zones ENABLE ROW LEVEL SECURITY;
ALTER TABLE shipping_zones FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON shipping_zones
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE shipping_zone_countries ENABLE ROW LEVEL SECURITY;
ALTER TABLE shipping_zone_countries FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON shipping_zone_countries
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE shipping_rates ENABLE ROW LEVEL SECURITY;
ALTER TABLE shipping_rates FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON shipping_rates
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
