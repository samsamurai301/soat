DELETE FROM price_list_prices WHERE min_quantity <> 1;
ALTER TABLE price_list_prices DROP CONSTRAINT price_list_prices_pkey;
ALTER TABLE price_list_prices ADD PRIMARY KEY (price_list_id, item_id);
ALTER TABLE price_list_prices DROP COLUMN min_quantity;
ALTER TABLE price_lists DROP COLUMN customer_tag;
