DROP INDEX apps_listed;
ALTER TABLE apps
    DROP COLUMN listed_at,
    DROP COLUMN app_url,
    DROP COLUMN category,
    DROP COLUMN description,
    DROP COLUMN tagline;
