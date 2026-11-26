-- The domains a shop is served on. A domain belongs to at most one shop on
-- the install. Requests are matched by their Host header before the shop is
-- known, so that lookup runs with `soat.system` on and may only read.
CREATE TABLE tenant_domains (
    domain TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX tenant_domains_tenant ON tenant_domains (tenant_id);

ALTER TABLE tenant_domains ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenant_domains FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tenant_domains
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
CREATE POLICY host_lookup ON tenant_domains FOR SELECT
    USING (current_setting('soat.system', true) = 'on');
