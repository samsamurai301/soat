-- C20: orders staff put together for a buyer, placed when complete.
CREATE TABLE draft_orders (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'completed')),
    order_id TEXT,
    customer_id TEXT,
    email TEXT NOT NULL DEFAULT '',
    currency TEXT NOT NULL,
    lines JSONB NOT NULL DEFAULT '[]',
    shipping_address JSONB,
    billing_address JSONB,
    vat_number TEXT,
    shipping_name TEXT,
    shipping_price_minor BIGINT NOT NULL DEFAULT 0 CHECK (shipping_price_minor >= 0),
    discount_minor BIGINT NOT NULL DEFAULT 0 CHECK (discount_minor >= 0),
    note TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX draft_orders_status ON draft_orders (tenant_id, status);

ALTER TABLE draft_orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE draft_orders FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON draft_orders
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
