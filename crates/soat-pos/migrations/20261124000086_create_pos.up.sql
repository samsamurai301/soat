-- A till at one of the shop's locations: what it sells leaves stock there.
CREATE TABLE pos_registers (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    location_id TEXT NOT NULL REFERENCES locations (id),
    currency TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A shift at a register, from opening with a float to the cash count.
CREATE TABLE pos_sessions (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    register_id TEXT NOT NULL REFERENCES pos_registers (id) ON DELETE CASCADE,
    opened_by TEXT,
    opening_cash_minor BIGINT NOT NULL CHECK (opening_cash_minor >= 0),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed')),
    counted_cash_minor BIGINT,
    closed_by TEXT,
    note TEXT NOT NULL DEFAULT '',
    opened_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at TIMESTAMPTZ
);
-- One open shift per register.
CREATE UNIQUE INDEX pos_sessions_open ON pos_sessions (tenant_id, register_id) WHERE status = 'open';

-- A sale rung up in a shift, and how it was paid.
CREATE TABLE pos_sales (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    order_id TEXT NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES pos_sessions (id) ON DELETE CASCADE,
    number BIGINT NOT NULL,
    total_minor BIGINT NOT NULL,
    cash_minor BIGINT NOT NULL DEFAULT 0,
    card_minor BIGINT NOT NULL DEFAULT 0,
    other_minor BIGINT NOT NULL DEFAULT 0,
    change_minor BIGINT NOT NULL DEFAULT 0,
    sold_by TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, order_id)
);
CREATE INDEX pos_sales_session ON pos_sales (tenant_id, session_id, created_at);

ALTER TABLE pos_registers ENABLE ROW LEVEL SECURITY;
ALTER TABLE pos_registers FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON pos_registers
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE pos_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE pos_sessions FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON pos_sessions
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE pos_sales ENABLE ROW LEVEL SECURITY;
ALTER TABLE pos_sales FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON pos_sales
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
