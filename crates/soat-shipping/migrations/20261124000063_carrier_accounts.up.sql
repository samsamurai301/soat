-- A shop's accounts with carriers (or a shipping platform such as
-- Sendcloud) that print labels. Credentials are sealed with the install's
-- secret key, bound to the shop.
CREATE TABLE carrier_accounts (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    name TEXT NOT NULL,
    settings JSONB NOT NULL DEFAULT '{}',
    credentials BYTEA NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

ALTER TABLE carrier_accounts ENABLE ROW LEVEL SECURITY;
ALTER TABLE carrier_accounts FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON carrier_accounts
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
