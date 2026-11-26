-- Which tax rates apply to a product (and its variants): a class such as
-- standard or reduced, resolved per country by the tax module.
ALTER TABLE products ADD COLUMN tax_class TEXT NOT NULL DEFAULT 'standard';
