-- A user may read their own memberships across shops, to list the shops
-- they work for. `soat.user` is set only for that query.
CREATE POLICY own_memberships ON memberships FOR SELECT
    USING (user_id = current_setting('soat.user', true));
