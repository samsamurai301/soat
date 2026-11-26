-- How a shop lets Claude (or any MCP client) change things.
CREATE TABLE assistant_settings (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    -- `writes`: every change waits for a person's approval; `off`: changes run at once.
    approval TEXT NOT NULL DEFAULT 'writes' CHECK (approval IN ('writes', 'off')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A change an assistant asked for, waiting for or past a person's decision.
CREATE TABLE assistant_proposals (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    tool TEXT NOT NULL,
    method TEXT NOT NULL,
    -- The API path with its query, e.g. `/api/products/prod_1`.
    path TEXT NOT NULL,
    body JSONB,
    -- Why the assistant wants it, in its words.
    reason TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'applied', 'failed', 'rejected')),
    proposed_by TEXT,
    decided_by TEXT,
    result_status INTEGER,
    result JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at TIMESTAMPTZ
);
CREATE INDEX assistant_proposals_by_status ON assistant_proposals (tenant_id, status, id);

ALTER TABLE assistant_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE assistant_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON assistant_settings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE assistant_proposals ENABLE ROW LEVEL SECURITY;
ALTER TABLE assistant_proposals FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON assistant_proposals
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
