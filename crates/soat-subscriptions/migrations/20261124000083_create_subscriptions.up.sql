-- How a product can be subscribed to: every few weeks or months.
CREATE TABLE subscription_plans (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    interval_unit TEXT NOT NULL CHECK (interval_unit IN ('week', 'month')),
    interval_count INTEGER NOT NULL CHECK (interval_count BETWEEN 1 AND 52),
    -- Off the goods of every order, in hundredths of a percent.
    discount_bps BIGINT NOT NULL DEFAULT 0 CHECK (discount_bps BETWEEN 0 AND 9000),
    shipping_price_minor BIGINT NOT NULL DEFAULT 0 CHECK (shipping_price_minor >= 0),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX subscription_plans_product ON subscription_plans (tenant_id, product_id);

-- A customer's subscription: an item sent again and again.
CREATE TABLE subscriptions (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    plan_id TEXT NOT NULL REFERENCES subscription_plans (id),
    item_id TEXT NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity BETWEEN 1 AND 1000),
    currency TEXT NOT NULL,
    shipping_address JSONB,
    billing_address JSONB,
    payment_method_id TEXT,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'canceled')),
    next_order_at TIMESTAMPTZ NOT NULL,
    orders_count BIGINT NOT NULL DEFAULT 0,
    last_order_id TEXT,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    canceled_at TIMESTAMPTZ
);
CREATE INDEX subscriptions_customer ON subscriptions (tenant_id, customer_id);
CREATE INDEX subscriptions_due ON subscriptions (tenant_id, next_order_at) WHERE status = 'active';

ALTER TABLE subscription_plans ENABLE ROW LEVEL SECURITY;
ALTER TABLE subscription_plans FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON subscription_plans
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
ALTER TABLE subscriptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE subscriptions FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON subscriptions
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
