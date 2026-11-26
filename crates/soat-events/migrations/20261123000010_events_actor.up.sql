-- Who caused the event: `user_...`, `key_...`, or NULL for the system.
-- With it the event log doubles as the shop's audit log.
ALTER TABLE events ADD COLUMN actor TEXT;
CREATE INDEX events_actor ON events (tenant_id, actor);
