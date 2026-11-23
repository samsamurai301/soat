-- Custom fields: what a shop defines per resource type (`product`), and the
-- values of each resource. Deleting a definition deletes its values.
CREATE TABLE field_definitions (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    resource TEXT NOT NULL,
    key TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    choices TEXT[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (tenant_id, resource, key)
);

CREATE TABLE field_values (
    tenant_id TEXT NOT NULL,
    resource TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value JSONB NOT NULL,
    PRIMARY KEY (tenant_id, resource, resource_id, key),
    FOREIGN KEY (tenant_id, resource, key)
        REFERENCES field_definitions (tenant_id, resource, key) ON DELETE CASCADE
);
CREATE INDEX field_values_key ON field_values (tenant_id, resource, key);

ALTER TABLE field_definitions ENABLE ROW LEVEL SECURITY;
ALTER TABLE field_definitions FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON field_definitions
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE field_values ENABLE ROW LEVEL SECURITY;
ALTER TABLE field_values FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON field_values
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
