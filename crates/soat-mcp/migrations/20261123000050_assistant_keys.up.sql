-- A shop's own Anthropic API key, sealed with the server's secret, and the
-- model the shop chose. The key itself is never shown again.
CREATE TABLE assistant_keys (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    sealed BYTEA NOT NULL,
    -- The key's last four characters, to tell keys apart.
    last4 TEXT NOT NULL,
    model TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE assistant_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE assistant_keys FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON assistant_keys
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
