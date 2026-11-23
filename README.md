# SOAT

A commerce engine that runs as one Rust binary: catalog, checkout, CMS and
admin behind a typed API, built on [Zapline](https://github.com/samsamurai301/zapline).

Status: foundation phase, aiming at a public alpha in the week of 4 January 2027.

## Run it

SOAT needs Postgres 16 or newer; it is the only service it needs.

```sh
docker run -d --name soat-pg -p 5432:5432 -e POSTGRES_HOST_AUTH_METHOD=trust postgres:16
createdb -h localhost -U postgres soat
DATABASE_URL=postgres://postgres@localhost/soat cargo run -p soat   # :8080, API docs at /docs
cargo test     # each test gets its own schema in a `soat_test` database
```

Tests connect with `SOAT_TEST_DATABASE_URL` (default
`postgres://postgres@localhost/postgres`) and create a non-superuser
`soat_test` role, so row-level security applies in tests as in production.

## Try the API

```sh
curl -X POST localhost:8080/api/tenants -H 'content-type: application/json' \
  -d '{"id":"acme","name":"Acme"}'
curl -X POST localhost:8080/api/products -H 'x-soat-tenant: acme' -H 'content-type: application/json' \
  -d '{"sku":"SHOE-1","title":"Trail shoe","price_minor":12999,"currency":"EUR"}'
curl localhost:8080/api/products -H 'x-soat-tenant: acme'
```

## Layout

| Path | What |
| --- | --- |
| `crates/soat-core` | Domain types with no HTTP or SQL: `Money`, `Currency`, `TenantId` |
| `crates/soat-module` | The `Module` trait and the `Modules` set that wires modules into one app |
| `crates/soat-tenancy` | Shops: the tenants table, `POST /api/tenants`, the `Tenant` extractor |
| `crates/soat-catalog` | Products of a shop |
| `crates/soat` | The server binary: lists the modules, end-to-end tests |

The full module map and milestones are in [docs/roadmap.md](docs/roadmap.md).

## What exists, what is next

- [x] Tenants, and products scoped to a tenant (`x-soat-tenant` header)
- [x] Money as integer minor units, validated currency codes
- [ ] Platform-admin auth before tenant creation is exposed anywhere public
- [x] Postgres as the only database
- [ ] Row-level security per tenant
- [ ] Variants, categories, media, inventory
- [ ] Cart, checkout, Stripe, orders
- [ ] Admin UI and storefront in Rust (Leptos or Zapline views)
- [ ] WASM plugin host, built into Zapline

## License

Not open source. See [LICENSE](LICENSE): SOAT's own terms are still being written.
