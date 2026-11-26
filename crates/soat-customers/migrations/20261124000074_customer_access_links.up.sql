-- Links that let a customer set a password: a reset, or activating an
-- email that ordered as a guest. The token is kept only until the email
-- is written; the hash is what a link is checked against.
CREATE TABLE customer_access_links (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (purpose IN ('reset', 'activate')),
    token_hash TEXT NOT NULL UNIQUE,
    token TEXT,
    link TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX customer_access_links_customer
    ON customer_access_links (tenant_id, customer_id, created_at);

ALTER TABLE customer_access_links ENABLE ROW LEVEL SECURITY;
ALTER TABLE customer_access_links FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON customer_access_links
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
