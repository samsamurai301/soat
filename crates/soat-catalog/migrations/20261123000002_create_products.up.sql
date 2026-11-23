-- Products belong to one tenant. Ids are public typed ids (`prod_...`).
-- Prices are integer minor units (cents).
CREATE TABLE products (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    sku TEXT NOT NULL,
    title TEXT NOT NULL,
    price_minor INTEGER NOT NULL CHECK (price_minor >= 0),
    currency TEXT NOT NULL,
    UNIQUE (tenant_id, sku)
);
