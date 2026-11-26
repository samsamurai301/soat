-- Custom roles a shop defines: a name and the permissions it grants.
-- `owner` and `staff` are built in and never stored here.
CREATE TABLE roles (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (name NOT IN ('owner', 'staff')),
    permissions TEXT[] NOT NULL,
    PRIMARY KEY (tenant_id, name)
);

ALTER TABLE roles ENABLE ROW LEVEL SECURITY;
ALTER TABLE roles FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON roles
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

-- A membership's role is now `owner`, `staff` or a custom role name.
ALTER TABLE memberships DROP CONSTRAINT memberships_role_check;
