-- A shop's cookie banner: whether it is shown, its version (a new one asks
-- everyone again) and the tracking it governs.
CREATE TABLE consent_settings (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    privacy_url TEXT NOT NULL DEFAULT '',
    ga4_id TEXT NOT NULL DEFAULT '',
    meta_pixel_id TEXT NOT NULL DEFAULT '',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Proof of each choice made in the banner (GDPR art. 7(1)), without who
-- made it: no address, no customer.
CREATE TABLE consent_records (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    version BIGINT NOT NULL,
    analytics BOOLEAN NOT NULL,
    marketing BOOLEAN NOT NULL,
    apps BOOLEAN NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX consent_records_at ON consent_records (tenant_id, created_at);

ALTER TABLE consent_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE consent_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON consent_settings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE consent_records ENABLE ROW LEVEL SECURITY;
ALTER TABLE consent_records FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON consent_records
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
