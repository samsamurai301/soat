-- Abandoned cart recovery: a shop turns it on and picks how long a cart
-- with an email sits untouched before the buyer gets one email with a
-- link that brings the cart back.
CREATE TABLE cart_recovery_settings (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    delay_minutes INTEGER NOT NULL DEFAULT 60 CHECK (delay_minutes BETWEEN 15 AND 10080),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE cart_recovery_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE cart_recovery_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON cart_recovery_settings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

-- When the email went out (once per cart), the link's hash, and the
-- link's token until the email has been written.
ALTER TABLE carts ADD COLUMN recovery_sent_at TIMESTAMPTZ;
ALTER TABLE carts ADD COLUMN recovery_token_hash TEXT UNIQUE;
ALTER TABLE carts ADD COLUMN recovery_token TEXT;
