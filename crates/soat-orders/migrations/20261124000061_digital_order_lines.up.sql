-- Whether a line ships; digital lines are delivered once the order is paid.
ALTER TABLE order_lines ADD COLUMN requires_shipping BOOLEAN NOT NULL DEFAULT TRUE;
