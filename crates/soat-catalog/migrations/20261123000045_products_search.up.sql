-- Full-text search over a product's title, SKU, tags and description.
-- array_to_string is only STABLE in general, but for text[] it is
-- immutable, which a generated column needs.
CREATE FUNCTION soat_tags_text(tags TEXT[]) RETURNS TEXT
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$ SELECT array_to_string(tags, ' ') $$;

ALTER TABLE products ADD COLUMN search_vector tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('simple', title), 'A')
    || setweight(to_tsvector('simple', sku || ' ' || soat_tags_text(tags)), 'B')
    || setweight(to_tsvector('simple', description), 'C')
) STORED;
CREATE INDEX products_search ON products USING GIN (search_vector);
