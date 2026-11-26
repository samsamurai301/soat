-- What the storefront needs: an address, a description, and whether to show it.
ALTER TABLE products
    ADD COLUMN handle TEXT,
    ADD COLUMN description TEXT NOT NULL DEFAULT '',
    ADD COLUMN status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'draft', 'archived'));

-- Existing products get a handle from their id (`prod_01…` → `prod-01…`).
-- Row-level security would hide every row from this update, so lift it briefly.
ALTER TABLE products NO FORCE ROW LEVEL SECURITY;
ALTER TABLE products DISABLE ROW LEVEL SECURITY;
UPDATE products SET handle = replace(id, '_', '-');
ALTER TABLE products ENABLE ROW LEVEL SECURITY;
ALTER TABLE products FORCE ROW LEVEL SECURITY;

ALTER TABLE products ALTER COLUMN handle SET NOT NULL;
ALTER TABLE products ADD CONSTRAINT products_handle_unique UNIQUE (tenant_id, handle);
CREATE INDEX products_status ON products (tenant_id, status);
