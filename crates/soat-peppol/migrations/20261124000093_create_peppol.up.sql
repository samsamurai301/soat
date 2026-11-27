-- The access point a shop sends through, and who it is on Peppol.
CREATE TABLE peppol_access_points (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    -- The shop's Peppol id, e.g. 0208:0123456749.
    sender_id TEXT NOT NULL,
    settings JSONB NOT NULL DEFAULT '{}',
    -- Sealed with the install's secret key.
    credentials BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Each time an invoice or credit note was handed to the access point.
CREATE TABLE peppol_transmissions (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    invoice_id TEXT NOT NULL REFERENCES invoices (id) ON DELETE CASCADE,
    invoice_number TEXT NOT NULL,
    provider TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    receiver_id TEXT NOT NULL,
    document_type TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('sent', 'failed')),
    -- The access point's id for it, once sent.
    reference TEXT,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX peppol_transmissions_invoice ON peppol_transmissions (tenant_id, invoice_id, created_at);
CREATE UNIQUE INDEX peppol_transmissions_sent_once ON peppol_transmissions (tenant_id, invoice_id, receiver_id)
    WHERE status = 'sent';

ALTER TABLE peppol_access_points ENABLE ROW LEVEL SECURITY;
ALTER TABLE peppol_access_points FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON peppol_access_points
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE peppol_transmissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE peppol_transmissions FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON peppol_transmissions
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
