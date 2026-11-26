-- F9: the plugins a shop turned on, with its settings; secret ones sealed.
CREATE TABLE shop_plugins (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT false,
    settings JSONB NOT NULL DEFAULT '{}',
    secrets BYTEA,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, name)
);

ALTER TABLE shop_plugins ENABLE ROW LEVEL SECURITY;
ALTER TABLE shop_plugins FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON shop_plugins
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
