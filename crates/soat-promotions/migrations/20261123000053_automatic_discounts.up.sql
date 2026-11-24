-- C14: automatic discounts (no code) and buy X get Y, optionally limited
-- to a collection's products.
ALTER TABLE discounts ALTER COLUMN code DROP NOT NULL;
ALTER TABLE discounts ADD COLUMN title TEXT NOT NULL DEFAULT '';
ALTER TABLE discounts ADD COLUMN automatic BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE discounts ADD COLUMN buy_quantity BIGINT CHECK (buy_quantity > 0);
ALTER TABLE discounts ADD COLUMN get_quantity BIGINT CHECK (get_quantity > 0);
ALTER TABLE discounts ADD COLUMN collection_id TEXT;
ALTER TABLE discounts DROP CONSTRAINT discounts_kind_check;
ALTER TABLE discounts ADD CONSTRAINT discounts_kind_check
    CHECK (kind IN ('percentage', 'fixed_amount', 'free_shipping', 'buy_x_get_y'));
ALTER TABLE discounts ADD CONSTRAINT discounts_code_unless_automatic
    CHECK ((code IS NULL) = automatic);
UPDATE discounts SET title = code;
CREATE INDEX discounts_automatic ON discounts (tenant_id) WHERE automatic;
