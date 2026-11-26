-- A shop's own version of a theme file; without a row the default is used.
CREATE TABLE theme_files (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    source TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, name)
);

-- A shop's theme settings: accent colour, announcement, ...
CREATE TABLE theme_settings (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    settings JSONB NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE theme_files ENABLE ROW LEVEL SECURITY;
ALTER TABLE theme_files FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON theme_files
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE theme_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE theme_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON theme_settings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
