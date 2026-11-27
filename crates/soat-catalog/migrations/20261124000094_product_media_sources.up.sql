-- Where a product's imported images came from, so importing again does
-- not fetch them twice (and does not bring back one staff removed).
CREATE TABLE product_media_sources (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    file_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, product_id, url)
);

ALTER TABLE product_media_sources ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_media_sources FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON product_media_sources
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
