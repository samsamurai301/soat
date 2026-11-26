-- How a shop's email goes out.
CREATE TABLE notification_settings (
    tenant_id TEXT PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
    -- Shown as the sender; empty: the shop's name.
    sender_name TEXT NOT NULL DEFAULT '',
    reply_to TEXT,
    -- Where new-order notices for staff go; null: none.
    staff_email TEXT,
    -- Templates not sent.
    disabled TEXT[] NOT NULL DEFAULT '{payment_received}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A shop's own wording of a template; without a row the built-in one is used.
CREATE TABLE email_templates (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    subject TEXT NOT NULL,
    text_body TEXT NOT NULL,
    html_body TEXT NOT NULL DEFAULT '',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, key)
);

-- Every email sent, or tried. One per event and template, so an event
-- handled twice sends once.
CREATE TABLE email_messages (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    template TEXT NOT NULL,
    event_id TEXT,
    order_id TEXT,
    recipient TEXT NOT NULL,
    subject TEXT NOT NULL,
    text_body TEXT NOT NULL,
    html_body TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at TIMESTAMPTZ,
    UNIQUE (tenant_id, event_id, template)
);
CREATE INDEX email_messages_order ON email_messages (tenant_id, order_id);

ALTER TABLE notification_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE notification_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON notification_settings
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE email_templates ENABLE ROW LEVEL SECURITY;
ALTER TABLE email_templates FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON email_templates
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));

ALTER TABLE email_messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE email_messages FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON email_messages
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
