ALTER TABLE carts DROP COLUMN recovery_token;
ALTER TABLE carts DROP COLUMN recovery_token_hash;
ALTER TABLE carts DROP COLUMN recovery_sent_at;
DROP TABLE cart_recovery_settings;
