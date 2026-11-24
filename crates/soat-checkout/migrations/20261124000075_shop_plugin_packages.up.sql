-- F10: plugins a shop uploads for itself: the manifest and the component.
CREATE TABLE shop_plugin_packages (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    manifest TEXT NOT NULL,
    component BYTEA NOT NULL,
    sha256 TEXT NOT NULL,
    size BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, name)
);

ALTER TABLE shop_plugin_packages ENABLE ROW LEVEL SECURITY;
ALTER TABLE shop_plugin_packages FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON shop_plugin_packages
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
