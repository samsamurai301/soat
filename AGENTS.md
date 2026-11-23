# Working on SOAT

SOAT is built by Sohang with Claude. These are the rules for any agent working here.

## Commands

- `cargo test`: all tests. Run it before every commit.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`: CI runs both.
- `cargo run -p soat`: the server on :8080, OpenAPI docs at `/docs`.

## Rules

- **Zapline first.** SOAT is built on Zapline (Sam's framework, tracked from its
  `main` branch). If SOAT needs something generic (plugin host, tenancy helpers,
  a new extractor), add it to Zapline, not here. Zapline's guide:
  https://samsamurai301.github.io/zapline/ (`llms.txt` there for agents).
- **Every tenant-owned table has `tenant_id`, and every query on it filters by
  it.** Take the `Tenant` extractor and call `tenant.verify(&db)` first. A
  foreign id must be a 404, never someone else's row. Add an isolation test for
  every new tenant-owned resource.
- **Money is integer minor units plus a `Currency`.** Never `f32`/`f64` for prices.
- **Domain logic goes in `soat-core`** when it needs no HTTP or SQL, with unit tests.
- **One module per file** in `crates/soat/src/routes/`, mounted in `routes::install`.
- **Migrations are reversible:** each `*.up.sql` has a `*.down.sql`. Never edit a
  migration that has shipped; add a new one.
- Handlers carry a doc comment: it becomes the OpenAPI description.
- Not open source: don't add third-party license headers or call it open source.

## Architecture

The plan, competitor analysis and MVP scope live in the SOAT architecture doc.
In short: one binary (API, admin, storefront, jobs), Postgres as the only
required service, WASM plugins with declared permissions, commerce and CMS in
one model, multi-tenant from day one.
