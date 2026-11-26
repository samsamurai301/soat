-- What a shopper entered at checkout for a cart.
CREATE TABLE checkouts (
    cart_id TEXT PRIMARY KEY REFERENCES carts (id) ON DELETE CASCADE,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    email TEXT,
    shipping_address JSONB,
    billing_address JSONB,
    vat_number TEXT,
    shipping_rate_id TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE checkouts ENABLE ROW LEVEL SECURITY;
ALTER TABLE checkouts FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON checkouts
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
