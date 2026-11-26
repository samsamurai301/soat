-- What search engines show: a title and description of their own, when
-- the shop sets them.
ALTER TABLE products
    ADD COLUMN seo_title TEXT NOT NULL DEFAULT '',
    ADD COLUMN seo_description TEXT NOT NULL DEFAULT '';
ALTER TABLE collections
    ADD COLUMN seo_title TEXT NOT NULL DEFAULT '',
    ADD COLUMN seo_description TEXT NOT NULL DEFAULT '';
