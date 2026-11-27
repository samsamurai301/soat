-- How long each shop keeps old rows, per retention rule a module declares
-- (`days`: rule key to days, or null to keep them; a missing key takes the
-- rule's default), and the last sweep.
CREATE TABLE retention_settings (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    days JSONB NOT NULL DEFAULT '{}',
    last_run_at TIMESTAMPTZ,
    last_report JSONB NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE retention_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE retention_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON retention_settings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
