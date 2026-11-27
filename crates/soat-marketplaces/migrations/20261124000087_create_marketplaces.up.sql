-- A marketplace the shop sells on: its prices and the stock it is offered.
CREATE TABLE marketplaces (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    channel TEXT NOT NULL,
    currency TEXT NOT NULL,
    -- Prices there: the shop's, plus this many basis points, plus this much.
    markup_bps BIGINT NOT NULL DEFAULT 0,
    markup_minor BIGINT NOT NULL DEFAULT 0,
    -- Units held back from what is offered, against selling twice.
    stock_buffer BIGINT NOT NULL DEFAULT 0 CHECK (stock_buffer >= 0),
    -- Offer every active product, or only the listed ones.
    all_products BOOLEAN NOT NULL DEFAULT TRUE,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

-- An item on a marketplace: its SKU and price there, or left out.
CREATE TABLE marketplace_listings (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    marketplace_id TEXT NOT NULL REFERENCES marketplaces (id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    external_sku TEXT NOT NULL DEFAULT '',
    price_minor BIGINT CHECK (price_minor >= 0),
    listed BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, marketplace_id, item_id)
);

-- An order taken on a marketplace, and what was last told back to it.
CREATE TABLE marketplace_orders (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    marketplace_id TEXT NOT NULL REFERENCES marketplaces (id) ON DELETE CASCADE,
    external_id TEXT NOT NULL,
    order_id TEXT NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    synced_state TEXT NOT NULL DEFAULT 'open',
    synced_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, marketplace_id, external_id)
);
CREATE INDEX marketplace_orders_order ON marketplace_orders (tenant_id, order_id);

ALTER TABLE marketplaces ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketplaces FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON marketplaces
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE marketplace_listings ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketplace_listings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON marketplace_listings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE marketplace_orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketplace_orders FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON marketplace_orders
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
