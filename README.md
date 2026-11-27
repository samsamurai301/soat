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
Set `SOAT_SECRET_KEY` (32 characters or more, the same on every restart) so
shops can store their own Anthropic key; it is kept encrypted with it.
Set `SOAT_METRICS_TOKEN` for Prometheus to scrape `/metrics`; logs, metrics
and health checks are described in [docs/operations.md](docs/operations.md).
Backups, restores and upgrades: [docs/backups.md](docs/backups.md).
Release builds and the container image: [docs/releases.md](docs/releases.md).

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

Staff can also chat with Claude in the admin (Assistant): SOAT calls Claude
with the shop's own Anthropic key (Settings, Assistant), offers it the same
tools, and shows proposed changes in the chat to approve. The chat API is
`/api/assistant/chats`; `SOAT_ANTHROPIC_API_BASE` points it at a proxy.

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
| `crates/soat-files` | Uploads checked by content, stored on disk or S3, images resized for `srcset` |
| `crates/soat-http` | The outbound HTTPS client adapters to outside services share |
| `crates/soat-catalog` | Products, variants, collections (by hand or by rules), categories, tags, product media |
| `crates/soat-inventory` | Locations, stock levels, reservations, adjustments and their ledger |
| `crates/soat-pricing` | Price lists per currency, customer groups and quantity tiers, compare-at prices, price resolution |
| `crates/soat-customers` | Customers, addresses, marketing consent |
| `crates/soat-cart` | Carts: storefront carts by token, live prices and stock |
| `crates/soat-tax` | Tax zones and rates per tax class, EU VAT (OSS, reverse charge), tax quotes, tax providers |
| `crates/soat-shipping` | Shipping zones and rates by order value and weight; carrier accounts and the label adapter trait |
| `crates/soat-payments` | Payment provider trait, payment methods, payments and refunds, manual payment |
| `crates/soat-stripe` | Stripe provider: PaymentIntents, Connect accounts, refunds, signed webhooks (feature `stripe`) |
| `crates/soat-mollie` | Mollie provider: iDEAL, Bancontact, cards, Klarna and more, refunds, webhooks checked with Mollie (feature `mollie`) |
| `crates/soat-paypal` | PayPal provider: Orders v2, capture or authorize, partner merchants, refunds, verified webhooks (feature `paypal`) |
| `crates/soat-sendcloud` | Sendcloud labels for DHL, DPD, GLS, PostNL, UPS and more through a shop's own account (feature `sendcloud`) |
| `crates/soat-apps` | Apps a shop finds in the directory and installs with consent: OAuth-style code for a scoped API key (feature `apps`) |
| `crates/soat-plugin-api` | Plugin interfaces: the `soat:plugin` WIT package and the plugin manifest |
| `crates/soat-plugin-host` | Runs WebAssembly plugins at checkout, sandboxed with fuel and memory caps |
| `crates/soat-cli` | The `soat app` commands: start an app from a template, register it and keep it in step while developing |
| `crates/soat-import` | Moving from Shopify without a gap: its store synced by the Admin API until cutover (feature `shopify-sync`) |
| `crates/soat-promotions` | Discount codes and automatic discounts: percentage, fixed amount, free shipping, buy X get Y, per collection, with dates, minimums and limits |
| `crates/soat-giftcards` | Gift cards and store credit: balances with a ledger, spent at checkout as payments, sold as products |
| `crates/soat-reviews` | Product reviews: buyers rate and review, staff moderate, the storefront shows them (feature `reviews`) |
| `crates/soat-wishlists` | Wishlists: customers save products for later, staff see the most saved (feature `wishlists`) |
| `crates/soat-loyalty` | Loyalty points and referrals, turned into store credit (feature `loyalty`) |
| `crates/soat-analytics` | Storefront analytics without cookies: visitors, pages, sources, devices, conversion (feature `analytics`) |
| `crates/soat-marketing` | Email campaigns to customers who agreed to news, with one-click unsubscribe (feature `marketing`) |
| `crates/soat-channels` | Sales channels: product feeds for Google Merchant Center and Meta catalogs, with a problems preview (feature `feeds`) |
| `crates/soat-b2b` | B2B: companies with shared prices, payment terms and a credit limit, quotes answered with draft orders, quick order by SKU (feature `b2b`) |
| `crates/soat-marketplaces` | Selling on marketplaces (Amazon, eBay, Otto, Kaufland and more): offers with a markup and a stock buffer, orders brought in, shipments and cancellations reported back (feature `marketplaces`, guide `docs/marketplaces.md`) |
| `crates/soat-pos` | Point of sale: registers at a location, sessions with a cash count, counter sales paid in cash or card with change, receipts (feature `pos`) |
| `crates/soat-compliance` | GDPR: a customer's data exported or erased across every module's declared tables, customers' requests, the cookie banner that holds tracking and apps' scripts until consent, and how long each kind of data is kept, swept daily (feature `privacy`, guide `docs/privacy.md`) |
| `crates/soat-subscriptions` | Subscriptions: plans on products, renewals placed as orders on schedule, pause, skip and cancel (feature `subscriptions`) |
| `crates/soat-orders` | Orders: snapshots, numbering, payment status, fulfillments, returns and refunds, cancellation, buyer views |
| `crates/soat-checkout` | Checkout: cart to order with shipping, tax, held stock and a payment |
| `crates/soat-invoices` | Invoices and credit notes: gapless numbers, totals per VAT rate, reverse charge, immutable, printable, and as e-invoices in XRechnung, ZUGFeRD / Factur-X XML and Peppol BIS (guide `docs/e-invoices.md`) |
| `crates/soat-peppol` | Peppol: sends invoices and credit notes to a buyer's Peppol id through the shop's access point (built-in signed `http` adapter, others through `AccessPoint`), every try kept (feature `peppol`, guide `docs/e-invoices.md`) |
| `crates/soat-notify` | Transactional email: order, shipping, cancel and refund emails, staff notices, editable templates, sent log |
| `crates/soat-cms` | Content: pages built from blocks (drafts and published), a blog with tags, scheduling and a feed, menus and URL redirects, read by the storefront |
| `crates/soat-theme` | Themes: a default theme, per-shop template and style edits, settings; safe templates with Markdown |
| `crates/soat-storefront` | The storefront: server-rendered pages from the theme, cart in a cookie, checkout, order and account pages, htmx |
| `crates/soat-mcp` | MCP server at `/mcp`: Claude runs a shop through a tool per API operation, scoped by API key, changes held for approval; the admin's chat with Claude on the shop's own key |
| `crates/soat` | The server binary: lists the modules, end-to-end tests |
| `crates/soat-testing` | Test helpers: a fresh Postgres schema per test, and accessibility checks of rendered pages (`a11y`, guide `docs/accessibility.md`) |

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
- [x] Customer group and quantity tier prices
- [x] Digital products: downloads once paid, nothing to ship
- [x] Bundles sold from their items' stock
- [x] Customers, with password reset and account activation by email
- [x] Cart
- [x] Tax zones and EU VAT
- [x] Shipping zones and rates
- [x] Payments core and manual payment
- [x] Stripe, Mollie and PayPal
- [x] Shipping labels through carrier accounts (Sendcloud)
- [x] Orders, draft orders and order edits
- [x] Checkout
- [x] Fulfillment, returns and refunds
- [x] Discount codes, automatic discounts and buy X get Y
- [x] Gift cards and store credit
- [x] Transactional email with editable templates
- [x] Invoices and credit notes
- [x] Content pages from blocks, and menus
- [x] Blog with tags, scheduled posts and an Atom feed
- [x] Storefront catalog API: active products and collections by handle
- [x] Claude runs your shop over MCP, with every change approved by a person
- [x] Storefront in Rust from an editable theme (server-rendered with htmx)
- [x] SEO basics: SEO fields, Open Graph, structured data, sitemap, robots.txt, redirects
- [x] Search with facets (tags, options, price) in Postgres full-text search
- [x] Checkout, order confirmation and customer account pages
- [x] Storefront in more languages: translated products, collections, pages, menus and theme texts under `/de/...`, hreflang, a language switcher
- [x] Markets: prices in more currencies by country, with exchange rates and rounding
- [x] Admin in the browser (htmx): sign in, pick a shop, manage products, orders, customers, content and the theme
- [x] Admin settings: general, payments, shipping, taxes, email wording, invoices, staff, API keys and webhooks, the assistant
- [x] Admin dashboard: orders waiting on staff, sales by day, best sellers, first steps for a new shop (`GET /api/orders/report`)
- [x] Setup in the browser: the first admin at `/admin/setup`, a new shop, then welcome steps for country and currency, EU VAT, bank transfer, shipping and the first product, or sample products from a store template (clothing, coffee, home goods) removed again in one click
- [x] CSV import and export of products (a row per variant, with stock) and customers, as spreadsheets save it (`;` or `,`, decimal commas)
- [x] Moving from Shopify: its product and customer exports import as they are (descriptions turned into Markdown; images not yet)
- [x] Moving from Shopify without a gap: keep its store synced until the day you cut over
- [x] Moving from WooCommerce or Shopware: their product exports (and Shopware's customer export) import as they are
- [x] Launch checklist: what a shop still needs before real orders, including the EU's legal pages (started as drafts to write)
- [x] Each shop's own Anthropic key, encrypted at rest (`SOAT_SECRET_KEY`)
- [x] Assistant in the admin: chat with Claude about the shop, approve its changes in the chat
- [x] Shop builder: describe the shop, review Claude's draft of products, collections and pages, build it
- [x] Content helpers: Claude suggests product copy and SEO fields, and translates products
- [x] Admin command palette (Ctrl+K) and keyboard shortcuts
- [x] Product variants and images in the admin
- [x] Responsive product images: smaller copies made on upload, served with `srcset`
- [x] Content types of the shop's own (store locations, size charts, FAQs) with typed fields, on the storefront API
- [x] Headless: the whole storefront as a JSON API in any language and market, callable from any origin ([guide](docs/headless.md))
- [x] Smart collections picked by rules, and collection screens in the admin
- [x] Apps: an app directory, install with consent for a key with exactly the granted permissions, admin links and storefront blocks, and `soat app new` to start one ([guide](docs/apps.md))
- [x] Tax by an outside tax service when zones are not enough, with the zones standing in when it is down ([guide](docs/tax-providers.md))
- [x] Hook points at checkout (shipping filters, discounts, order validation) for native hooks today and plugins later; plugin interfaces as WIT ([guide](docs/plugins.md))
- [x] WASM plugin host: plugins from `SOAT_PLUGINS_DIR` run at checkout, sandboxed and metered, for the shops that turn them on (admin Settings, Plugins), and shops can upload their own ([guide](docs/plugins.md)); moves into Zapline later

## License

Not open source. See [LICENSE](LICENSE): SOAT's own terms are still being written.
