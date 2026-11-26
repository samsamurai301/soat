-- Shipping weight in grams, used by weight-based shipping rates. Variants
-- share their product's weight for now.
ALTER TABLE products ADD COLUMN weight_grams BIGINT CHECK (weight_grams >= 0);
