-- C15: the gift card a buyer entered, and whether to spend store credit.
ALTER TABLE checkouts ADD COLUMN gift_card_id TEXT;
ALTER TABLE checkouts ADD COLUMN use_store_credit BOOLEAN NOT NULL DEFAULT FALSE;
