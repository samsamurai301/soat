-- A customer's request to get or erase their data, until staff act on it.
-- It names the customer by id only, so it outlives their erasure.
CREATE TABLE privacy_requests (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('erase')),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'done', 'declined')),
    note TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX privacy_requests_open ON privacy_requests (tenant_id, customer_id, kind)
    WHERE status = 'open';

ALTER TABLE privacy_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE privacy_requests FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON privacy_requests
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
