-- Gift cards bought as products: the order line and unit each was issued
-- for (one card per unit, once), and the code until it has been emailed.
ALTER TABLE gift_cards ADD COLUMN order_id TEXT;
ALTER TABLE gift_cards ADD COLUMN order_line_id TEXT;
ALTER TABLE gift_cards ADD COLUMN line_unit INTEGER;
ALTER TABLE gift_cards ADD COLUMN recipient_email TEXT;
ALTER TABLE gift_cards ADD COLUMN unsent_code TEXT;
CREATE UNIQUE INDEX gift_cards_order_line
    ON gift_cards (tenant_id, order_line_id, line_unit) WHERE order_line_id IS NOT NULL;
