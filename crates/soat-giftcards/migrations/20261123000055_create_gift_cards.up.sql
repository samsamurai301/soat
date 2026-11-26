-- Gift cards and store credit: a balance in one currency. A gift card is
-- found by its code (only a hash is kept); store credit belongs to a
-- customer, one per currency.
CREATE TABLE gift_cards (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('gift_card', 'store_credit')),
    code_hash TEXT,
    last_chars TEXT NOT NULL DEFAULT '',
    currency TEXT NOT NULL,
    initial_minor BIGINT NOT NULL CHECK (initial_minor >= 0),
    balance_minor BIGINT NOT NULL CHECK (balance_minor >= 0),
    customer_id TEXT,
    expires_at TIMESTAMPTZ,
    note TEXT NOT NULL DEFAULT '',
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((kind = 'gift_card') = (code_hash IS NOT NULL)),
    CHECK (kind = 'gift_card' OR customer_id IS NOT NULL),
    UNIQUE (tenant_id, code_hash)
);
CREATE UNIQUE INDEX gift_cards_store_credit
    ON gift_cards (tenant_id, customer_id, currency) WHERE kind = 'store_credit';
CREATE INDEX gift_cards_customer ON gift_cards (tenant_id, customer_id);

-- Every change of a balance.
CREATE TABLE gift_card_transactions (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    gift_card_id TEXT NOT NULL REFERENCES gift_cards (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('issued', 'adjusted', 'redeemed', 'refunded')),
    amount_minor BIGINT NOT NULL,
    balance_minor BIGINT NOT NULL,
    order_id TEXT,
    -- The payment refund that gave money back, once.
    refund_id TEXT UNIQUE,
    note TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX gift_card_transactions_card ON gift_card_transactions (gift_card_id, id);

ALTER TABLE gift_cards ENABLE ROW LEVEL SECURITY;
ALTER TABLE gift_cards FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON gift_cards
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE gift_card_transactions ENABLE ROW LEVEL SECURITY;
ALTER TABLE gift_card_transactions FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON gift_card_transactions
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
