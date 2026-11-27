-- A business that buys from the shop: its people share prices, terms
-- and a credit limit.
CREATE TABLE companies (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    vat_number TEXT,
    -- Members get this customer tag, so the price lists for it apply.
    price_tag TEXT,
    -- Days an order may stay unpaid; 0 is pay now.
    payment_terms_days INTEGER NOT NULL DEFAULT 0 CHECK (payment_terms_days BETWEEN 0 AND 180),
    currency TEXT NOT NULL,
    -- Most the company may owe at once; none is no limit.
    credit_limit_minor BIGINT CHECK (credit_limit_minor >= 0),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'on_hold')),
    note TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX companies_tenant ON companies (tenant_id, name);

-- The customers who buy for a company; a customer is in one at most.
CREATE TABLE company_members (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    company_id TEXT NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'buyer' CHECK (role IN ('admin', 'buyer')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, customer_id)
);
CREATE INDEX company_members_company ON company_members (tenant_id, company_id);

-- What a company owes: each of its members' orders, due after its terms.
CREATE TABLE company_orders (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    order_id TEXT NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    company_id TEXT NOT NULL REFERENCES companies (id) ON DELETE CASCADE,
    number BIGINT NOT NULL,
    total_minor BIGINT NOT NULL,
    currency TEXT NOT NULL,
    placed_at TIMESTAMPTZ NOT NULL,
    due_at TIMESTAMPTZ NOT NULL,
    paid_at TIMESTAMPTZ,
    -- Canceled or refunded in full: owes nothing.
    closed BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (tenant_id, order_id)
);
CREATE INDEX company_orders_company ON company_orders (tenant_id, company_id, placed_at);

-- A buyer asks for a price; staff answer with a draft order.
CREATE TABLE quotes (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    company_id TEXT REFERENCES companies (id) ON DELETE SET NULL,
    lines JSONB NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'offered', 'accepted', 'declined')),
    draft_order_id TEXT,
    order_id TEXT,
    expires_at TIMESTAMPTZ,
    reply TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX quotes_tenant ON quotes (tenant_id, status, id);
CREATE INDEX quotes_customer ON quotes (tenant_id, customer_id, id);

ALTER TABLE companies ENABLE ROW LEVEL SECURITY;
ALTER TABLE companies FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON companies
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE company_members ENABLE ROW LEVEL SECURITY;
ALTER TABLE company_members FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON company_members
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE company_orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE company_orders FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON company_orders
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE quotes ENABLE ROW LEVEL SECURITY;
ALTER TABLE quotes FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON quotes
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
