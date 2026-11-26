-- Where a shop sells from and the currency it prices in; the admin uses
-- them as defaults. Empty until the owner sets them up.
ALTER TABLE tenants ADD COLUMN country TEXT, ADD COLUMN currency TEXT;
