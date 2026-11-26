-- Conversations staff have with Claude in the admin, each with the
-- messages exactly as sent to Claude (so its thinking stays valid), the
-- system prompt frozen when the chat began, and which tool calls became
-- proposals.
CREATE TABLE assistant_chats (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    -- `user_...` or `key_...`: only they see the chat.
    started_by TEXT NOT NULL,
    title TEXT NOT NULL,
    system TEXT NOT NULL,
    messages JSONB NOT NULL DEFAULT '[]',
    -- Tool call id to proposal id.
    proposals JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX assistant_chats_started_by ON assistant_chats (tenant_id, started_by, id);

ALTER TABLE assistant_chats ENABLE ROW LEVEL SECURITY;
ALTER TABLE assistant_chats FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON assistant_chats
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
