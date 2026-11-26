-- D12: content types a shop defines (store locations, size charts, FAQs)
-- and their entries. A type with entries cannot be deleted.
CREATE TABLE content_types (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    name TEXT NOT NULL,
    fields JSONB NOT NULL DEFAULT '[]',
    public BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, key)
);

CREATE TABLE content_entries (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    type_key TEXT NOT NULL,
    handle TEXT NOT NULL,
    fields JSONB NOT NULL DEFAULT '{}',
    published_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, type_key, handle),
    FOREIGN KEY (tenant_id, type_key) REFERENCES content_types (tenant_id, key)
);

ALTER TABLE content_types ENABLE ROW LEVEL SECURITY;
ALTER TABLE content_types FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON content_types
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE content_entries ENABLE ROW LEVEL SECURITY;
ALTER TABLE content_entries FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON content_entries
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
