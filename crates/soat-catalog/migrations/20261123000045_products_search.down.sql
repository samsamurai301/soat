DROP INDEX products_search;
ALTER TABLE products DROP COLUMN search_vector;
DROP FUNCTION soat_tags_text(TEXT[]);
