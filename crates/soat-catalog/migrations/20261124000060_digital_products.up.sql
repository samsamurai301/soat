-- Digital products: nothing to ship, and files the buyer downloads once
-- the order is paid.
ALTER TABLE products ADD COLUMN requires_shipping BOOLEAN NOT NULL DEFAULT TRUE;

CREATE TABLE product_downloads (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    file_id TEXT NOT NULL REFERENCES files (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (product_id, file_id)
);
CREATE INDEX product_downloads_file ON product_downloads (file_id);

ALTER TABLE product_downloads ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_downloads FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON product_downloads
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
