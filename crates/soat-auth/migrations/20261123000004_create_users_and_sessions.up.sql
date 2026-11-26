-- People who sign in: platform admins and shop staff. Not tenant-owned: one
-- person can work for several shops (see tenancy's memberships).
CREATE TABLE users (
    id TEXT PRIMARY KEY,
    -- Stored lowercased.
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    is_platform_admin BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Login sessions. Only the SHA-256 of the bearer token is stored.
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX sessions_user_id ON sessions (user_id);
