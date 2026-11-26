-- Placed orders. Everything a buyer agreed to is copied in (lines,
-- prices, tax, addresses), so later catalog changes never alter them.
CREATE TABLE orders (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    -- Shown to people: 1001, 1002, ... per shop.
    number BIGINT NOT NULL,
    -- SHA-256 of the token a guest uses to see the order.
    token_hash TEXT NOT NULL UNIQUE,
    cart_id TEXT,
    customer_id TEXT REFERENCES customers (id) ON DELETE SET NULL,
    email TEXT NOT NULL,
    currency TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'canceled')),
    financial_status TEXT NOT NULL DEFAULT 'pending',
    fulfillment_status TEXT NOT NULL DEFAULT 'unfulfilled',
    subtotal_minor BIGINT NOT NULL,
    shipping_minor BIGINT NOT NULL DEFAULT 0,
    tax_minor BIGINT NOT NULL DEFAULT 0,
    discount_minor BIGINT NOT NULL DEFAULT 0,
    total_minor BIGINT NOT NULL CHECK (total_minor >= 0),
    prices_include_tax BOOLEAN NOT NULL,
    tax_country TEXT,
    reverse_charge BOOLEAN NOT NULL DEFAULT FALSE,
    vat_number TEXT,
    shipping_address JSONB,
    billing_address JSONB,
    shipping_rate_id TEXT,
    shipping_name TEXT,
    shipping_tax_minor BIGINT NOT NULL DEFAULT 0,
    note TEXT NOT NULL DEFAULT '',
    cancel_reason TEXT,
    placed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    canceled_at TIMESTAMPTZ,
    UNIQUE (tenant_id, number)
);
CREATE INDEX orders_customer ON orders (customer_id);
CREATE INDEX orders_placed ON orders (tenant_id, placed_at);

CREATE TABLE order_lines (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    order_id TEXT NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    item_id TEXT NOT NULL,
    product_id TEXT,
    sku TEXT NOT NULL,
    title TEXT NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    unit_price_minor BIGINT NOT NULL,
    total_minor BIGINT NOT NULL,
    tax_class TEXT NOT NULL,
    tax_rate_bps BIGINT NOT NULL,
    tax_minor BIGINT NOT NULL
);
CREATE INDEX order_lines_order ON order_lines (order_id, position);

-- The last order number each shop handed out.
CREATE TABLE order_counters (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    last BIGINT NOT NULL
);

ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE orders FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON orders
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE order_lines ENABLE ROW LEVEL SECURITY;
ALTER TABLE order_lines FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON order_lines
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE order_counters ENABLE ROW LEVEL SECURITY;
ALTER TABLE order_counters FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON order_counters
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
