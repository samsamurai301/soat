-- A customer with a password has an account and can sign in to the shop.
ALTER TABLE customers ADD COLUMN password_hash TEXT;

-- Storefront sign-ins; only the SHA-256 of the token is kept.
CREATE TABLE customer_sessions (
    token_hash TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX customer_sessions_customer ON customer_sessions (customer_id);

ALTER TABLE customer_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE customer_sessions FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON customer_sessions
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
