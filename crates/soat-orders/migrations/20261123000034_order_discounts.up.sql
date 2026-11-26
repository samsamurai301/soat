-- Discounts on orders: the code used, and each line's share of it (so a
-- return refunds what the buyer actually paid).
ALTER TABLE orders ADD COLUMN discount_code TEXT;
ALTER TABLE order_lines ADD COLUMN discount_minor BIGINT NOT NULL DEFAULT 0;
