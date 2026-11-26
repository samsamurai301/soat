-- What invoices need about shipping: its tax rate and what a code took off.
ALTER TABLE orders ADD COLUMN shipping_tax_rate_bps BIGINT NOT NULL DEFAULT 0;
ALTER TABLE orders ADD COLUMN shipping_discount_minor BIGINT NOT NULL DEFAULT 0;
