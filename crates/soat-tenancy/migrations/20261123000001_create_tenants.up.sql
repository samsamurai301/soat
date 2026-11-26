-- One row per shop on this install. Every tenant-owned table references it.
CREATE TABLE tenants (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
