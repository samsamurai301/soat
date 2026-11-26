-- D7: blog posts in Markdown, published now or at a set time.
CREATE TABLE blog_posts (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    handle TEXT NOT NULL,
    title TEXT NOT NULL,
    excerpt TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL DEFAULT '',
    image_url TEXT,
    author TEXT NOT NULL DEFAULT '',
    tags TEXT[] NOT NULL DEFAULT '{}',
    seo_title TEXT,
    seo_description TEXT,
    published_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, handle)
);
CREATE INDEX blog_posts_published ON blog_posts (tenant_id, published_at DESC);

ALTER TABLE blog_posts ENABLE ROW LEVEL SECURITY;
ALTER TABLE blog_posts FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON blog_posts
    USING (tenant_id = current_setting('soat.tenant', true))
    WITH CHECK (tenant_id = current_setting('soat.tenant', true));
