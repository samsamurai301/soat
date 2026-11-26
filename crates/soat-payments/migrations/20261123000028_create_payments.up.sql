-- The ways a shop takes money: a provider (manual, stripe, ...) with its
-- non-secret settings.
CREATE TABLE payment_methods (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    name TEXT NOT NULL,
    settings JSONB NOT NULL DEFAULT '{}',
    active BOOLEAN NOT NULL DEFAULT TRUE,
    position INTEGER NOT NULL DEFAULT 0,
    UNIQUE (tenant_id, name)
);

-- One attempt to collect an amount for a reference (an order, a cart).
CREATE TABLE payments (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    reference TEXT NOT NULL,
    method_id TEXT REFERENCES payment_methods (id) ON DELETE SET NULL,
    provider TEXT NOT NULL,
    provider_ref TEXT,
    amount_minor BIGINT NOT NULL CHECK (amount_minor > 0),
    currency TEXT NOT NULL,
    status TEXT NOT NULL,
    refunded_minor BIGINT NOT NULL DEFAULT 0 CHECK (refunded_minor >= 0),
    next_action JSONB,
    failure_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (refunded_minor <= amount_minor)
);
CREATE INDEX payments_reference ON payments (tenant_id, reference);
CREATE UNIQUE INDEX payments_provider_ref ON payments (provider, provider_ref);

-- Money returned from a payment.
CREATE TABLE payment_refunds (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    payment_id TEXT NOT NULL REFERENCES payments (id) ON DELETE CASCADE,
    amount_minor BIGINT NOT NULL CHECK (amount_minor > 0),
    reason TEXT NOT NULL DEFAULT '',
    provider_ref TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX payment_refunds_payment ON payment_refunds (payment_id);

ALTER TABLE payment_methods ENABLE ROW LEVEL SECURITY;
ALTER TABLE payment_methods FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON payment_methods
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE payments ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON payments
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE payment_refunds ENABLE ROW LEVEL SECURITY;
ALTER TABLE payment_refunds FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON payment_refunds
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
