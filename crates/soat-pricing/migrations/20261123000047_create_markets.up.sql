-- Markets: groups of countries that buy in one currency. With an exchange
-- rate, items priced in the base currency are converted when no price
-- list prices them in the market's currency.
CREATE TABLE markets (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    countries TEXT[] NOT NULL,
    currency TEXT NOT NULL,
    base_currency TEXT,
    -- Units of `currency` per unit of `base_currency`, in millionths.
    exchange_rate_ppm BIGINT CHECK (exchange_rate_ppm > 0),
    rounding TEXT NOT NULL DEFAULT 'exact',
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name),
    CHECK ((base_currency IS NULL) = (exchange_rate_ppm IS NULL))
);
CREATE INDEX markets_currency ON markets (tenant_id, currency);

ALTER TABLE markets ENABLE ROW LEVEL SECURITY;
ALTER TABLE markets FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON markets
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
