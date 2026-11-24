-- C5: a shop's tax provider (an adapter with sealed credentials), and the
-- quotes it gave, kept a few minutes so checkout pages do not ask again.
CREATE TABLE tax_provider_accounts (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    settings JSONB NOT NULL DEFAULT '{}',
    credentials BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE tax_quote_cache (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    lines JSONB NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tenant_id, key)
);

ALTER TABLE tax_provider_accounts ENABLE ROW LEVEL SECURITY;
ALTER TABLE tax_provider_accounts FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tax_provider_accounts
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE tax_quote_cache ENABLE ROW LEVEL SECURITY;
ALTER TABLE tax_quote_cache FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tax_quote_cache
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
