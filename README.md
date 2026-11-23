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

On first start the server prints a setup token (or set `SOAT_SETUP_TOKEN`).
Use it once to create the platform admin, then act with the returned token.
Uploads go to `./storage` (`STORAGE_DIR`), or to S3 when `S3_BUCKET` is set;
set `STORAGE_SECRET` so file links survive restarts.
Email goes out over SMTP (`SMTP_URL`, e.g. `smtps://user:pass@smtp.example.com`)
from `MAIL_FROM`, shown with each shop's name. Without `SMTP_URL` a debug build
keeps mail in a dev inbox at `/_dev/mail`; a release build refuses to start.

```sh
TOKEN=$(curl -s -X POST localhost:8080/api/setup -H 'content-type: application/json' \
  -d '{"setup_token":"<from the log>","email":"you@example.com","password":"a long password"}' | jq -r .token)
curl -X POST localhost:8080/api/tenants -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"id":"acme","name":"Acme"}'
curl -X POST localhost:8080/api/products -H "authorization: Bearer $TOKEN" -H 'x-soat-tenant: acme' \
  -H 'content-type: application/json' \
  -d '{"sku":"SHOE-1","title":"Trail shoe","price_minor":12999,"currency":"EUR"}'
curl localhost:8080/api/products -H "authorization: Bearer $TOKEN" -H 'x-soat-tenant: acme'
```

## Run your shop with Claude

SOAT is an MCP server at `/mcp`: every API operation is a tool. Give Claude an
API key with `assistant:use` and the permissions it should have, then connect it:

```sh
KEY=$(curl -s -X POST localhost:8080/api/api-keys -H "authorization: Bearer $TOKEN" \
  -H 'x-soat-tenant: acme' -H 'content-type: application/json' \
  -d '{"name":"claude","permissions":["assistant:use","products:read","products:write","orders:read"]}' | jq -r .secret)
claude mcp add --transport http soat http://localhost:8080/mcp --header "Authorization: Bearer $KEY"
```

Claude reads freely. A change waits for a person until someone with
`assistant:approve` approves it (`POST /api/assistant/proposals/{id}/approve`;
it then runs with the approver's permissions) or rejects it. Owners can let
changes run at once with `PUT /api/assistant/settings {"approval":"off"}`.

## Layout

| Path | What |
| --- | --- |
| `crates/soat-core` | Domain rules with no HTTP or SQL: money, ids, handles, options, custom field kinds, media types |
| `crates/soat-module` | The `Module` trait and the `Modules` set that wires modules into one app |
| `crates/soat-auth` | Users, platform admins, login sessions, first-run setup, the `Staff` extractor |
| `crates/soat-events` | Domain events: transactional outbox, at-least-once subscribers |
| `crates/soat-tenancy` | Shops, staff, roles, API keys, domains, the audit log, the `Tenant` extractor |
| `crates/soat-webhooks` | Signed outbound webhooks with retries and a delivery log |
| `crates/soat-fields` | Custom fields any resource can carry |
| `crates/soat-files` | Uploads checked by content, stored on disk or S3 |
| `crates/soat-catalog` | Products, variants, collections, categories, tags, product media |
| `crates/soat-inventory` | Locations, stock levels, reservations, adjustments and their ledger |
| `crates/soat-pricing` | Price lists per currency, compare-at prices, price resolution |
| `crates/soat-customers` | Customers, addresses, marketing consent |
| `crates/soat-cart` | Carts: storefront carts by token, live prices and stock |
| `crates/soat-tax` | Tax zones and rates per tax class, EU VAT (OSS, reverse charge), tax quotes |
| `crates/soat-shipping` | Shipping zones and rates by order value and weight |
| `crates/soat-payments` | Payment provider trait, payment methods, payments and refunds, manual payment |
| `crates/soat-stripe` | Stripe provider: PaymentIntents, Connect accounts, refunds, signed webhooks (feature `stripe`) |
| `crates/soat-mollie` | Mollie provider: iDEAL, Bancontact, cards, Klarna and more, refunds, webhooks checked with Mollie (feature `mollie`) |
| `crates/soat-paypal` | PayPal provider: Orders v2, capture or authorize, partner merchants, refunds, verified webhooks (feature `paypal`) |
| `crates/soat-promotions` | Discount codes: percentage, fixed amount, free shipping, with dates, minimums and limits |
| `crates/soat-orders` | Orders: snapshots, numbering, payment status, fulfillments, returns and refunds, cancellation, buyer views |
| `crates/soat-checkout` | Checkout: cart to order with shipping, tax, held stock and a payment |
| `crates/soat-invoices` | Invoices and credit notes: gapless numbers, totals per VAT rate, reverse charge, immutable, printable |
| `crates/soat-notify` | Transactional email: order, shipping, cancel and refund emails, staff notices, editable templates, sent log |
| `crates/soat-cms` | Content: pages built from blocks (drafts and published), menus and URL redirects, read by the storefront |
| `crates/soat-theme` | Themes: a default theme, per-shop template and style edits, settings; safe templates with Markdown |
| `crates/soat-storefront` | The storefront: server-rendered pages from the theme, cart in a cookie, checkout, order and account pages, htmx |
| `crates/soat-mcp` | MCP server at `/mcp`: Claude runs a shop through a tool per API operation, scoped by API key, changes held for approval |
| `crates/soat` | The server binary: lists the modules, end-to-end tests |
| `crates/soat-testing` | Test helpers: a fresh Postgres schema per test |

The full module map and milestones are in [docs/roadmap.md](docs/roadmap.md).

## What exists, what is next

- [x] Tenants, and products scoped to a tenant (`x-soat-tenant` header)
- [x] Money as integer minor units, validated currency codes
- [x] Platform admin, staff login, shop creation behind auth
- [x] Postgres as the only database
- [x] Row-level security per tenant
- [x] Roles, API keys, events, webhooks, audit log, shop domains
- [x] Variants, collections, categories, tags, custom fields, media
- [x] Inventory and locations
- [x] Stock reservations
- [x] Price lists and currencies
- [x] Customers
- [x] Cart
- [x] Tax zones and EU VAT
- [x] Shipping zones and rates
- [x] Payments core and manual payment
- [x] Stripe, Mollie and PayPal
- [x] Orders
- [x] Checkout
- [x] Fulfillment, returns and refunds
- [x] Discount codes
- [x] Transactional email with editable templates
- [x] Invoices and credit notes
- [x] Content pages from blocks, and menus
- [x] Storefront catalog API: active products and collections by handle
- [x] Claude runs your shop over MCP, with every change approved by a person
- [x] Storefront in Rust from an editable theme (server-rendered with htmx)
- [x] SEO basics: SEO fields, Open Graph, structured data, sitemap, robots.txt, redirects
- [x] Checkout, order confirmation and customer account pages
- [ ] Admin UI (htmx)
- [ ] WASM plugin host, built into Zapline

## License

Not open source. See [LICENSE](LICENSE): SOAT's own terms are still being written.
