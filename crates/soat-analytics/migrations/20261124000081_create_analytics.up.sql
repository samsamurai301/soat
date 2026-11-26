-- Storefront page views. No cookies and no personal data: a visitor is a
-- hash of the day's salt, the shop, the address and the browser, so the
-- same person on another day is someone else.
CREATE TABLE analytics_hits (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    at TIMESTAMPTZ NOT NULL DEFAULT now(),
    visitor TEXT NOT NULL,
    path TEXT NOT NULL,
    -- The theme page shown, e.g. product.
    page TEXT NOT NULL,
    -- utm_source, or the referring site; empty for direct visits.
    source TEXT NOT NULL DEFAULT '',
    device TEXT NOT NULL DEFAULT 'desktop' CHECK (device IN ('desktop', 'mobile', 'tablet'))
);
CREATE INDEX analytics_hits_at ON analytics_hits (tenant_id, at);

ALTER TABLE analytics_hits ENABLE ROW LEVEL SECURITY;
ALTER TABLE analytics_hits FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON analytics_hits
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

-- Today's salt for visitor hashes. Older salts are deleted, so visitors
-- of past days cannot be linked to anyone.
CREATE TABLE analytics_salts (
    day DATE PRIMARY KEY,
    salt TEXT NOT NULL
);
