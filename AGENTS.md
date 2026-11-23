# Working on SOAT

SOAT is built by Sohang with Claude. These are the rules for any agent working here.

## Commands

- `cargo test`: all tests. Run it before every commit. Needs Postgres:
  `SOAT_TEST_DATABASE_URL` (default `postgres://postgres@localhost/postgres`);
  end-to-end tests use `tests/common::client()`, one fresh schema per test.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`: CI runs both.
- `cargo run -p soat`: the server on :8080, OpenAPI docs at `/docs`.

## Rules

- **Zapline first.** SOAT is built on Zapline (Sam's framework, tracked from its
  `main` branch). If SOAT needs something generic (plugin host, tenancy helpers,
  a new extractor), add it to Zapline, not here. Zapline's guide:
  https://samsamurai301.github.io/zapline/ (`llms.txt` there for agents).
- **Every tenant-owned table has `tenant_id`, row-level security, and every
  query on it filters by it.** Take the `Tenant` extractor, open
  `tenant.begin(&db)` and run every query on that transaction (`&mut *tx`),
  then commit. Lists use `TenantListing`. The table's migration enables and
  forces RLS with the `tenant_isolation` policy (copy it from the catalog's
  `..._products_row_level_security.up.sql`); a test fails for any `tenant_id`
  table without it. A foreign id must be a 404, never someone else's row. Add
  an API isolation test for every new tenant-owned resource.
- **Postgres only.** Integer columns that map to `i64` are `BIGINT`. Ids are
  `TEXT` typed ids (`soat_core::Id`), generated in Rust.
- **Money is integer minor units plus a `Currency`.** Never `f32`/`f64` for prices.
- **Domain logic goes in `soat-core`** when it needs no HTTP or SQL, with unit tests.
- **One domain = one crate.** Each domain is `crates/soat-<name>`, exports one
  value implementing `soat_module::Module` (name, `depends_on`, migrations in
  its own `migrations/`, routes) and is listed in `soat::modules()`. The `soat`
  crate holds no domain code. Create a crate when its milestone starts, never
  as an empty placeholder.
- **Dependencies point down.** A module may depend on foundation crates
  (`soat-core`, `soat-module`, `soat-tenancy`) and on modules it names in
  `depends_on`. It never imports a sibling in its own layer; siblings talk
  through events. Anything a shop can live without goes behind a Cargo feature.
- **Migration versions are timestamps, unique across all crates**, because every
  module shares one migrations table. `soat::modules().validate()` rejects
  clashes.
- **Migrations are reversible:** each `*.up.sql` has a `*.down.sql`. Never edit a
  migration that has shipped; add a new one.
- Handlers carry a doc comment: it becomes the OpenAPI description.
- Not open source: don't add third-party license headers or call it open source.

## Architecture

The plan, competitor analysis and MVP scope live in the SOAT architecture doc.
The module map and milestone list live in `docs/roadmap.md`: tick a milestone
there in the PR that finishes it.
In short: one binary (API, admin, storefront, jobs), Postgres as the only
required service, WASM plugins with declared permissions, commerce and CMS in
one model, multi-tenant from day one.
