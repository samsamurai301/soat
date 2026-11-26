-- Who issues a shop's invoices, and how they are numbered.
CREATE TABLE invoice_settings (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    legal_name TEXT NOT NULL,
    address JSONB NOT NULL,
    vat_number TEXT,
    tax_number TEXT,
    email TEXT,
    bank_details TEXT NOT NULL DEFAULT '',
    footer TEXT NOT NULL DEFAULT '',
    invoice_prefix TEXT NOT NULL DEFAULT 'INV-',
    credit_note_prefix TEXT NOT NULL DEFAULT 'CN-',
    -- When an order's invoice is issued by itself.
    issue_on TEXT NOT NULL DEFAULT 'paid' CHECK (issue_on IN ('placed', 'paid', 'manual')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The last number used per series and year: numbers have no gaps.
CREATE TABLE invoice_counters (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    series TEXT NOT NULL,
    year INTEGER NOT NULL,
    last BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, series, year)
);

-- Invoices and credit notes, as issued. They never change: a mistake is
-- corrected by a credit note.
CREATE TABLE invoices (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('invoice', 'credit_note')),
    number TEXT NOT NULL,
    order_id TEXT NOT NULL,
    order_number BIGINT NOT NULL,
    -- For a credit note, the invoice it credits.
    credits_id TEXT REFERENCES invoices (id),
    -- For a credit note made for a refund.
    refund_id TEXT,
    issued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    currency TEXT NOT NULL,
    seller JSONB NOT NULL,
    buyer JSONB NOT NULL,
    lines JSONB NOT NULL,
    rates JSONB NOT NULL,
    net_minor BIGINT NOT NULL,
    tax_minor BIGINT NOT NULL,
    total_minor BIGINT NOT NULL,
    prices_include_tax BOOLEAN NOT NULL,
    reverse_charge BOOLEAN NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    UNIQUE (tenant_id, number)
);
CREATE UNIQUE INDEX invoices_one_per_order ON invoices (tenant_id, order_id) WHERE kind = 'invoice';
CREATE UNIQUE INDEX invoices_one_per_refund ON invoices (tenant_id, refund_id) WHERE refund_id IS NOT NULL;
CREATE INDEX invoices_credits ON invoices (credits_id);

CREATE FUNCTION invoices_are_final() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'invoices cannot be changed once issued';
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER invoices_are_final BEFORE UPDATE ON invoices
    FOR EACH ROW EXECUTE FUNCTION invoices_are_final();

ALTER TABLE invoice_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE invoice_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON invoice_settings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE invoice_counters ENABLE ROW LEVEL SECURITY;
ALTER TABLE invoice_counters FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON invoice_counters
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE invoices ENABLE ROW LEVEL SECURITY;
ALTER TABLE invoices FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON invoices
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
