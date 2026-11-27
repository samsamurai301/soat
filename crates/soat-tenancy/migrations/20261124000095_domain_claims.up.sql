-- Domains a shop asked for but has not proved it owns yet. Any number of
-- shops may claim a domain; the first whose DNS TXT record matches gets it
-- in tenant_domains, so nobody can hold a domain they do not own.
CREATE TABLE domain_claims (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    -- What the TXT record at _soat-challenge.<domain> must say.
    token TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, domain)
);

ALTER TABLE domain_claims ENABLE ROW LEVEL SECURITY;
ALTER TABLE domain_claims FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON domain_claims
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
