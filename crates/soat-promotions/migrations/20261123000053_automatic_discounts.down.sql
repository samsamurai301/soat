DROP INDEX discounts_automatic;
DELETE FROM discounts WHERE automatic OR kind = 'buy_x_get_y';
ALTER TABLE discounts DROP CONSTRAINT discounts_code_unless_automatic;
ALTER TABLE discounts DROP CONSTRAINT discounts_kind_check;
ALTER TABLE discounts ADD CONSTRAINT discounts_kind_check
    CHECK (kind IN ('percentage', 'fixed_amount', 'free_shipping'));
ALTER TABLE discounts DROP COLUMN collection_id;
ALTER TABLE discounts DROP COLUMN get_quantity;
ALTER TABLE discounts DROP COLUMN buy_quantity;
ALTER TABLE discounts DROP COLUMN automatic;
ALTER TABLE discounts DROP COLUMN title;
ALTER TABLE discounts ALTER COLUMN code SET NOT NULL;
