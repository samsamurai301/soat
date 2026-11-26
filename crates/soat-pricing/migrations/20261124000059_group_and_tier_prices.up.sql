-- A price list can be for customers with one tag (a customer group), and
-- a price can start at a quantity (a tier). The best qualifying price of
-- the highest-priority list wins.
ALTER TABLE price_lists ADD COLUMN customer_tag TEXT;
ALTER TABLE price_list_prices
    ADD COLUMN min_quantity INTEGER NOT NULL DEFAULT 1 CHECK (min_quantity BETWEEN 1 AND 100000);
ALTER TABLE price_list_prices DROP CONSTRAINT price_list_prices_pkey;
ALTER TABLE price_list_prices ADD PRIMARY KEY (price_list_id, item_id, min_quantity);
