-- Units held for a cart or an order (`reference`) until released,
-- committed or expired. Available = on hand - active reservations.
CREATE TABLE inventory_reservations (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    quantity BIGINT NOT NULL CHECK (quantity > 0),
    reference TEXT NOT NULL,
    -- NULL holds until released.
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX inventory_reservations_item ON inventory_reservations (tenant_id, item_id);
CREATE INDEX inventory_reservations_reference ON inventory_reservations (tenant_id, reference);

ALTER TABLE inventory_reservations ENABLE ROW LEVEL SECURITY;
ALTER TABLE inventory_reservations FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON inventory_reservations
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
