-- Where a shop keeps stock: warehouses, stores, a supplier's dropship.
CREATE TABLE locations (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    address_line1 TEXT NOT NULL DEFAULT '',
    address_line2 TEXT NOT NULL DEFAULT '',
    postal_code TEXT NOT NULL DEFAULT '',
    city TEXT NOT NULL DEFAULT '',
    -- ISO 3166-1 alpha-2, e.g. DE.
    country TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    UNIQUE (tenant_id, name)
);

-- How a stock item (a variant, or a product without variants) is stocked.
-- No row means the defaults: tracked, no backorders.
CREATE TABLE inventory_items (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    tracked BOOLEAN NOT NULL DEFAULT TRUE,
    allow_backorder BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (tenant_id, item_id)
);

-- Units on hand per item and location.
CREATE TABLE inventory_levels (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    location_id TEXT NOT NULL REFERENCES locations (id) ON DELETE CASCADE,
    on_hand BIGINT NOT NULL CHECK (on_hand >= 0),
    PRIMARY KEY (tenant_id, item_id, location_id)
);
CREATE INDEX inventory_levels_location ON inventory_levels (location_id, on_hand);

-- Every change of on-hand stock, kept after the item is gone.
CREATE TABLE inventory_adjustments (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    location_id TEXT NOT NULL,
    delta BIGINT NOT NULL,
    on_hand_after BIGINT NOT NULL,
    reason TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    actor TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX inventory_adjustments_item ON inventory_adjustments (tenant_id, item_id, id);

ALTER TABLE locations ENABLE ROW LEVEL SECURITY;
ALTER TABLE locations FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON locations
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE inventory_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE inventory_items FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON inventory_items
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE inventory_levels ENABLE ROW LEVEL SECURITY;
ALTER TABLE inventory_levels FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON inventory_levels
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE inventory_adjustments ENABLE ROW LEVEL SECURITY;
ALTER TABLE inventory_adjustments FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON inventory_adjustments
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
