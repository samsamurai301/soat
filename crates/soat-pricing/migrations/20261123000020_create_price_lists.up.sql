-- Prices in a currency, overriding an item's own price. The active list
-- with the highest priority that prices an item wins.
CREATE TABLE price_lists (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    currency TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    UNIQUE (tenant_id, name)
);

-- One item's price in a list. `item_id` is a variant, or a product without
-- variants. `compare_at_minor` is the struck-through "was" price.
CREATE TABLE price_list_prices (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    price_list_id TEXT NOT NULL REFERENCES price_lists (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    price_minor BIGINT NOT NULL CHECK (price_minor >= 0),
    compare_at_minor BIGINT CHECK (compare_at_minor > price_minor),
    PRIMARY KEY (price_list_id, item_id)
);
CREATE INDEX price_list_prices_item ON price_list_prices (tenant_id, item_id);

ALTER TABLE price_lists ENABLE ROW LEVEL SECURITY;
ALTER TABLE price_lists FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON price_lists
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE price_list_prices ENABLE ROW LEVEL SECURITY;
ALTER TABLE price_list_prices FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON price_list_prices
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
