-- A shop's loyalty program.
CREATE TABLE loyalty_settings (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    points_per_unit BIGINT NOT NULL DEFAULT 1,
    points_per_credit BIGINT NOT NULL DEFAULT 100,
    min_redeem BIGINT NOT NULL DEFAULT 100,
    referrer_points BIGINT NOT NULL DEFAULT 500,
    referee_points BIGINT NOT NULL DEFAULT 250,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A customer's points and referral code.
CREATE TABLE loyalty_accounts (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    balance BIGINT NOT NULL DEFAULT 0 CHECK (balance >= 0),
    referral_code TEXT NOT NULL,
    referred_by TEXT REFERENCES customers (id) ON DELETE SET NULL,
    referral_rewarded BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, customer_id),
    UNIQUE (tenant_id, referral_code)
);

-- Every change of a customer's points.
CREATE TABLE loyalty_entries (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    kind TEXT NOT NULL
        CHECK (kind IN ('earned', 'reversed', 'redeemed', 'referral', 'welcome', 'adjusted')),
    points BIGINT NOT NULL,
    balance BIGINT NOT NULL,
    order_id TEXT,
    note TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX loyalty_entries_customer ON loyalty_entries (tenant_id, customer_id, id);
-- An order moves a customer's points once per kind.
CREATE UNIQUE INDEX loyalty_entries_order
    ON loyalty_entries (tenant_id, customer_id, order_id, kind) WHERE order_id IS NOT NULL;

ALTER TABLE loyalty_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE loyalty_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON loyalty_settings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
ALTER TABLE loyalty_accounts ENABLE ROW LEVEL SECURITY;
ALTER TABLE loyalty_accounts FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON loyalty_accounts
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
ALTER TABLE loyalty_entries ENABLE ROW LEVEL SECURITY;
ALTER TABLE loyalty_entries FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON loyalty_entries
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
