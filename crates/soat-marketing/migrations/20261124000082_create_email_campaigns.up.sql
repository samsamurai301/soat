-- Email campaigns: news a shop sends to customers who agreed to it.
CREATE TABLE email_campaigns (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    subject TEXT NOT NULL,
    text_body TEXT NOT NULL,
    html_body TEXT NOT NULL DEFAULT '',
    -- Only customers with this tag; null for all who agreed to news.
    audience_tag TEXT,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'sending', 'sent', 'canceled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ
);
CREATE INDEX email_campaigns_tenant ON email_campaigns (tenant_id, id);
CREATE INDEX email_campaigns_sending ON email_campaigns (tenant_id) WHERE status = 'sending';

-- Who a campaign goes to, fixed when it is sent.
CREATE TABLE campaign_recipients (
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    campaign_id TEXT NOT NULL REFERENCES email_campaigns (id) ON DELETE CASCADE,
    customer_id TEXT NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    error TEXT,
    -- SHA-256 of the unsubscribe link's token; the token is only in the email.
    unsubscribe_hash TEXT UNIQUE,
    sent_at TIMESTAMPTZ,
    unsubscribed_at TIMESTAMPTZ,
    PRIMARY KEY (campaign_id, customer_id)
);
CREATE INDEX campaign_recipients_pending
    ON campaign_recipients (tenant_id, campaign_id) WHERE status = 'pending';

ALTER TABLE email_campaigns ENABLE ROW LEVEL SECURITY;
ALTER TABLE email_campaigns FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON email_campaigns
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
ALTER TABLE campaign_recipients ENABLE ROW LEVEL SECURITY;
ALTER TABLE campaign_recipients FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON campaign_recipients
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
