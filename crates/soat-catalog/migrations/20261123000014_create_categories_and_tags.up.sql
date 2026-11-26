-- Free-form tags on products, filterable with `?tag=`.
ALTER TABLE products ADD COLUMN tags TEXT[] NOT NULL DEFAULT '{}';
CREATE INDEX products_tags ON products USING GIN (tags);

-- A tree of categories. `ancestors` lists the ids from the root down to the
-- category itself, so a subtree is `WHERE $id = ANY(ancestors)`.
CREATE TABLE categories (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    parent_id TEXT REFERENCES categories (id) ON DELETE CASCADE,
    ancestors TEXT[] NOT NULL,
    handle TEXT NOT NULL,
    title TEXT NOT NULL,
    UNIQUE (tenant_id, handle)
);
CREATE INDEX categories_ancestors ON categories USING GIN (ancestors);

CREATE TABLE product_categories (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    category_id TEXT NOT NULL REFERENCES categories (id) ON DELETE CASCADE,
    PRIMARY KEY (product_id, category_id)
);
CREATE INDEX product_categories_category ON product_categories (category_id);

ALTER TABLE categories ENABLE ROW LEVEL SECURITY;
ALTER TABLE categories FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON categories
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE product_categories ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_categories FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON product_categories
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
