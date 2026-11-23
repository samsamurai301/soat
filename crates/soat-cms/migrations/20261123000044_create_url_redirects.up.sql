-- Old addresses on the shop that send buyers (and search engines) on with
-- a 301: added by staff, or when a product or collection changes handle.
CREATE TABLE url_redirects (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    from_path TEXT NOT NULL,
    to_url TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, from_path)
);
CREATE INDEX url_redirects_to ON url_redirects (tenant_id, to_url);

ALTER TABLE url_redirects ENABLE ROW LEVEL SECURITY;
ALTER TABLE url_redirects FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON url_redirects
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
