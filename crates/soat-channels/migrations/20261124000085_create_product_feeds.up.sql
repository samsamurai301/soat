-- A product feed for a channel: Google Merchant Center or a Meta catalog
-- fetch it from its URL, which carries a secret token.
CREATE TABLE product_feeds (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('google', 'meta')),
    currency TEXT NOT NULL,
    -- The token in the feed's URL: it only reads public product data.
    token TEXT NOT NULL UNIQUE,
    -- Shown on every item; the shop's name when empty.
    brand TEXT NOT NULL DEFAULT '',
    -- Only this collection's products; all when NULL.
    collection_handle TEXT,
    include_out_of_stock BOOLEAN NOT NULL DEFAULT TRUE,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    fetched_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX product_feeds_tenant ON product_feeds (tenant_id, id);

ALTER TABLE product_feeds ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_feeds FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON product_feeds
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
