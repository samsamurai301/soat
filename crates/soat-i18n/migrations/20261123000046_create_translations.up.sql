-- The languages a shop publishes its storefront in.
CREATE TABLE shop_languages (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    default_language TEXT NOT NULL,
    -- Besides the default.
    languages TEXT[] NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One translated field of one resource, e.g. a product's German title.
CREATE TABLE translations (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    language TEXT NOT NULL,
    field TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, resource_type, resource_id, language, field)
);
CREATE INDEX translations_language ON translations (tenant_id, language, resource_type);

ALTER TABLE shop_languages ENABLE ROW LEVEL SECURITY;
ALTER TABLE shop_languages FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON shop_languages
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE translations ENABLE ROW LEVEL SECURITY;
ALTER TABLE translations FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON translations
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
