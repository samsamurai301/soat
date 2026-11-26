-- A product's images and videos, in order: the first is its main image.
CREATE TABLE product_media (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    file_id TEXT NOT NULL REFERENCES files (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (product_id, file_id)
);
CREATE INDEX product_media_file ON product_media (file_id);

ALTER TABLE product_media ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_media FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON product_media
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
