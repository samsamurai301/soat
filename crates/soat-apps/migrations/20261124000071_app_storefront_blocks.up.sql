-- F7: blocks an app offers the storefront, and which ones each shop
-- turned on.
ALTER TABLE apps ADD COLUMN storefront_blocks JSONB NOT NULL DEFAULT '[]';
ALTER TABLE app_installations ADD COLUMN enabled_blocks TEXT[] NOT NULL DEFAULT '{}';
