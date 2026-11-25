DROP INDEX gift_cards_order_line;
ALTER TABLE gift_cards DROP COLUMN unsent_code;
ALTER TABLE gift_cards DROP COLUMN recipient_email;
ALTER TABLE gift_cards DROP COLUMN line_unit;
ALTER TABLE gift_cards DROP COLUMN order_line_id;
ALTER TABLE gift_cards DROP COLUMN order_id;
