-- Payments a module of SOAT took itself (gift cards, store credit); no
-- provider is asked to refund them.
ALTER TABLE payments ADD COLUMN recorded BOOLEAN NOT NULL DEFAULT FALSE;
