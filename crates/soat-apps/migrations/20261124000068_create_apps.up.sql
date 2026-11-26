-- F4: apps developers register on this install, shops' installs of them
-- with the permissions granted, and the one-time codes an app trades for
-- its key.
CREATE TABLE apps (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    redirect_uris TEXT[] NOT NULL,
    permissions TEXT[] NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX apps_owner ON apps (owner_id);

CREATE TABLE app_installations (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    app_id TEXT NOT NULL REFERENCES apps (id) ON DELETE CASCADE,
    permissions TEXT[] NOT NULL,
    installed_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    api_key_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, app_id)
);

CREATE TABLE app_codes (
    code_hash TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    installation_id TEXT NOT NULL REFERENCES app_installations (id) ON DELETE CASCADE,
    redirect_uri TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

ALTER TABLE app_installations ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_installations FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON app_installations
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE app_codes ENABLE ROW LEVEL SECURITY;
ALTER TABLE app_codes FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON app_codes
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
