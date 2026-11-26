-- Which users work for which shop, and as what. `owner` may manage staff;
-- roles and permissions grow in milestone A6.
CREATE TABLE memberships (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'staff')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);

ALTER TABLE memberships ENABLE ROW LEVEL SECURITY;
ALTER TABLE memberships FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON memberships
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
