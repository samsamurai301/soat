-- F6: links an app adds to the admin: to the app itself, or next to a
-- product, order or customer.
ALTER TABLE apps ADD COLUMN admin_links JSONB NOT NULL DEFAULT '[]';
