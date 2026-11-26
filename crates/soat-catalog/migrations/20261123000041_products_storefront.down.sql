DROP INDEX products_status;
ALTER TABLE products DROP CONSTRAINT products_handle_unique;
ALTER TABLE products DROP COLUMN status, DROP COLUMN description, DROP COLUMN handle;
