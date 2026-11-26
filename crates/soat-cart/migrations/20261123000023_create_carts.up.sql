-- Storefront carts. A guest holds the cart by its token (only the SHA-256
-- is kept); a signed-in customer can attach it to their account.
CREATE TABLE carts (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    customer_id TEXT REFERENCES customers (id) ON DELETE SET NULL,
    currency TEXT NOT NULL,
    email TEXT NOT NULL DEFAULT '',
    -- open, or completed once checkout turned it into an order.
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'completed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX carts_customer ON carts (customer_id);
CREATE INDEX carts_updated ON carts (tenant_id, status, updated_at);

-- What is in a cart. Prices are not stored: they are resolved when the cart
-- is read, so a cart always shows today's price.
CREATE TABLE cart_lines (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    cart_id TEXT NOT NULL REFERENCES carts (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity BETWEEN 1 AND 999),
    UNIQUE (cart_id, item_id)
);

ALTER TABLE carts ENABLE ROW LEVEL SECURITY;
ALTER TABLE carts FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON carts
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE cart_lines ENABLE ROW LEVEL SECURITY;
ALTER TABLE cart_lines FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON cart_lines
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
