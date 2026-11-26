UPDATE memberships SET role = 'staff' WHERE role NOT IN ('owner', 'staff');
ALTER TABLE memberships ADD CONSTRAINT memberships_role_check CHECK (role IN ('owner', 'staff'));
DROP TABLE roles;
