-- How a shop charges tax. One row per shop; a missing row means the
-- defaults (prices include tax, no origin, origin VAT inside the EU).
CREATE TABLE tax_settings (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    -- Where the shop is registered for tax (ISO 3166-1 alpha-2).
    origin_country TEXT,
    prices_include_tax BOOLEAN NOT NULL DEFAULT TRUE,
    -- EU One-Stop Shop: charge the buyer's country's VAT on EU sales.
    charge_destination_vat BOOLEAN NOT NULL DEFAULT FALSE
);

-- A set of countries sharing tax rates.
CREATE TABLE tax_zones (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    UNIQUE (tenant_id, name)
);

-- A country belongs to at most one zone of a shop.
CREATE TABLE tax_zone_countries (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    zone_id TEXT NOT NULL REFERENCES tax_zones (id) ON DELETE CASCADE,
    country TEXT NOT NULL,
    PRIMARY KEY (tenant_id, country)
);
CREATE INDEX tax_zone_countries_zone ON tax_zone_countries (zone_id);

-- A zone's rate per tax class, in basis points (1900 = 19%).
CREATE TABLE tax_rates (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    zone_id TEXT NOT NULL REFERENCES tax_zones (id) ON DELETE CASCADE,
    tax_class TEXT NOT NULL,
    rate_bps BIGINT NOT NULL CHECK (rate_bps BETWEEN 0 AND 10000),
    name TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (zone_id, tax_class)
);

ALTER TABLE tax_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE tax_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tax_settings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE tax_zones ENABLE ROW LEVEL SECURITY;
ALTER TABLE tax_zones FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tax_zones
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE tax_zone_countries ENABLE ROW LEVEL SECURITY;
ALTER TABLE tax_zone_countries FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tax_zone_countries
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE tax_rates ENABLE ROW LEVEL SECURITY;
ALTER TABLE tax_rates FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tax_rates
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
