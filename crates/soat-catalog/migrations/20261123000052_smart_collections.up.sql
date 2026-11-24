-- Smart collections: rules pick a collection's products, kept up to date
-- as products change. NULL rules: the merchant picks by hand.
ALTER TABLE collections
    ADD COLUMN rules JSONB,
    ADD COLUMN match_all BOOLEAN NOT NULL DEFAULT true;
