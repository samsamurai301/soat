-- Discount codes.
CREATE TABLE discounts (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    code TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('percentage', 'fixed_amount', 'free_shipping')),
    -- Basis points for a percentage, minor units for a fixed amount.
    value BIGINT NOT NULL DEFAULT 0 CHECK (value >= 0),
    -- The currency of a fixed amount and of the minimum; null: any.
    currency TEXT,
    min_subtotal_minor BIGINT CHECK (min_subtotal_minor >= 0),
    starts_at TIMESTAMPTZ,
    ends_at TIMESTAMPTZ,
    usage_limit BIGINT CHECK (usage_limit > 0),
    once_per_customer BOOLEAN NOT NULL DEFAULT FALSE,
    used_count BIGINT NOT NULL DEFAULT 0,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, code)
);

-- Each use of a code by an order.
CREATE TABLE discount_redemptions (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    discount_id TEXT NOT NULL REFERENCES discounts (id) ON DELETE CASCADE,
    order_id TEXT NOT NULL,
    customer_id TEXT,
    email TEXT NOT NULL,
    amount_minor BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (discount_id, order_id)
);
CREATE INDEX discount_redemptions_email ON discount_redemptions (discount_id, email);

ALTER TABLE discounts ENABLE ROW LEVEL SECURITY;
ALTER TABLE discounts FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON discounts
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE discount_redemptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE discount_redemptions FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON discount_redemptions
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
