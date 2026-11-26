-- F8: what an app shows in the directory, and where a shop starts
-- installing it. An app is listed while listed_at is set.
ALTER TABLE apps
    ADD COLUMN tagline TEXT NOT NULL DEFAULT '',
    ADD COLUMN description TEXT NOT NULL DEFAULT '',
    ADD COLUMN category TEXT NOT NULL DEFAULT '',
    ADD COLUMN app_url TEXT,
    ADD COLUMN listed_at TIMESTAMPTZ;
CREATE INDEX apps_listed ON apps (listed_at) WHERE listed_at IS NOT NULL;
