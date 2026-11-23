# SOAT roadmap

The working copy of the micro-plan: one line per milestone, one milestone per
PR. Tick a line in the PR that finishes it. The reasoning, the full feature
catalogue and open questions are in the plan doc
(https://claude.ai/artifact/2Rzd1CxY5h8vJygFPXjNeY).

Lines marked **alpha** are in the public alpha (week of 2027-01-04).

## Layers

Each layer may use only the layers below it. Siblings in one layer talk
through events, never by importing each other. Foundation crates are a stack
(core, module, db, auth, events, tenancy, webhooks, fields, files, bottom up); in the catalog layer, inventory and
pricing build on the catalog; selling is a stack too (customers; then cart,
tax, shipping, payments and promotions side by side; orders; checkout;
notifications).
`crates/soat/tests/layers.rs` enforces this and fails for a crate without a
layer.

| Layer | Crates (created when their milestone starts) |
| --- | --- |
| Surfaces | `soat-storefront`, `soat-admin`, REST API, `soat-mcp` |
| Extensions (Cargo features) | `soat-apps`, `soat-plugin-api`, `soat-plugin-host`, `soat-cli`, `soat-onboarding`, `soat-import`, `soat-ai`, `soat-analytics`, `soat-marketing`, `soat-subscriptions`, `soat-b2b`, `soat-channels`, `soat-compliance` |
| Content | `soat-cms`, `soat-i18n`, `soat-seo`, `soat-search`, `soat-theme` |
| Selling | `soat-customers`, `soat-cart`, `soat-tax`, `soat-shipping`, `soat-payments`, `soat-checkout`, `soat-orders`, `soat-promotions`, `soat-notify` |
| Catalog | `soat-catalog`, `soat-inventory`, `soat-pricing` |
| Foundation | `soat-core`, `soat-module`, `soat-tenancy`, `soat-db`, `soat-events`, `soat-auth`, `soat-webhooks`, `soat-fields`, `soat-files` |
| Zapline + Postgres | gains tenancy, outbox, webhooks, plugin host and MCP as SOAT needs them |

## A. Foundation

- [x] A1 Module system: `soat-module`, tenancy and catalog as module crates, per-module migrations **alpha**
- [x] A2 Typed IDs (`prod_…`) and list endpoints (Zapline `Listing`: offset pages, sort, search, filters) **alpha**; cursor pagination later as a Zapline addition
- [x] A3 Postgres as the only database; tests get a fresh schema each, CI runs a Postgres service **alpha**
- [x] A4 Row-level security per shop (`Tenant::begin`, `TenantListing`), a test that fails for any unprotected `tenant_id` table **alpha**
- [x] A5 First-run setup token, platform admin, staff accounts and memberships, bearer sessions; shop creation behind auth **alpha**
- [x] A6 Permissions declared per module, built-in owner and staff roles, custom roles, no granting beyond your own permissions **alpha**
- [x] A7 Shop API keys (`soat_sk_<shop>_…`) scoped by permissions, shown once, revocable, never beyond their creator **alpha**
- [x] A8 Domain events: transactional outbox, at-least-once subscribers with backoff, `GET /api/events`, `product.created` **alpha**
- [x] A9 Webhooks: endpoints per shop, HMAC-signed POSTs, retries with backoff, delivery log, public-HTTPS-only targets **alpha**
- [x] A10 Audit log: every event records its actor (user or API key) and time; staff, roles, keys, webhooks and shops publish events **alpha**
- [x] A11 Shop resolution: API key, then `x-soat-tenant`, then the `Host` among the shop's domains (`/api/domains`); domain ownership checks (DNS TXT) later **alpha**
- [x] A12 CI check for forbidden crate dependencies (`tests/layers.rs`, from `cargo metadata`)

## B. Catalog

- [x] B1 Variants and options: up to 3 options per product, variants with own SKU and price (product currency), options change only while variants still fit **alpha**
- [x] B2 Collections: hand-picked, ordered, unique handles per shop **alpha**
- [ ] B3 Smart collections
- [x] B4 Categories (a tree, up to 8 deep, listing includes subtrees) and product tags (`?tag=`) **alpha**
- [x] B5 Custom fields: `soat-fields` (foundation), typed definitions per resource type, product values at `/api/products/{id}/fields` **alpha**
- [x] B6 Files and media: `soat-files` (foundation) on Zapline storage, type from content (no HTML/SVG), signed links, ordered product media **alpha**
- [ ] B7 Image pipeline
- [x] B8 Inventory and locations: stock per variant (or variant-less product) and location, adjustments with reasons, counts, transfers, a ledger **alpha**
- [x] B9 Stock reservations: by reference (cart or order), optional expiry, locked per item so nothing oversells **alpha**
- [x] B10 Price lists and currencies: lists per currency with priority, compare-at prices, `GET /api/prices/{item}?currency=` resolution **alpha**
- [ ] B11 Group and tier prices
- [ ] B12 Digital products and bundles

## C. Selling

- [x] C1 Customers: profiles, unique email per shop, marketing consent with time, tags, addresses with one default, custom fields **alpha**
- [x] C2 Customer login: storefront register, sign in, sign out, own profile; `Shop` extractor for public requests; claiming a known email waits for C16 **alpha**
- [x] C3 Cart: token-held storefront carts, live prices and stock per line, customers claim carts, staff list them **alpha**
- [x] C4 Tax zones and rates, EU VAT: zones per country set, rates per tax class in basis points, products carry a class, EU preset, origin or destination (OSS) VAT, reverse charge, `POST /api/tax/quote` **alpha**
- [ ] C5 Tax provider trait
- [x] C6 Shipping zones and rates: zones of countries, rates per currency with inclusive value and weight bounds, delivery estimates, product weights, `POST /api/shipping/options` **alpha**
- [x] C7 Payments core and manual payment: `PaymentProvider` trait registered on the module, payment methods per shop, payments with a status machine, capture, cancel, refunds, manual provider with instructions and mark-paid **alpha**
- [x] C8 Stripe: PaymentIntents with one platform key and optional Connect account per method, client secret handed to the buyer once and never stored, manual capture, cancel, refunds, signed webhooks checked against the stored intent; behind the `stripe` feature **alpha**
- [x] C9 Checkout: email, addresses, VAT number and shipping choice per cart, live totals with tax and what is missing, completion places the order, holds stock (an hour for card payments, until paid otherwise), closes the cart and starts the payment **alpha**
- [x] C10 Orders: `place()` copies lines, tax, shipping and addresses in, per-shop numbers from 1001, financial status follows payments through a subscriber, cancel releases held stock, guest token and customer account views **alpha**
- [x] C11 Fulfillment: ship some or all of an order's lines from a chosen or best-stocked location (recorded as `sold`, holds released), tracking, delivered, cancel a shipment to restock, order fulfillment status **alpha**
- [x] C12 Refunds and returns: customers or staff request returns of shipped units, staff receive (restock where they shipped from, or not), reject, or refund them through the order's payments with optional shipping; goodwill refunds of any amount across payments **alpha**
- [x] C13 Discount codes: percentage, fixed amount or free shipping, with currency, minimum subtotal, start and end dates, a usage limit and once per customer; entered at checkout, shared over the lines before tax, recorded on the order and redeemed with it **alpha**
- [ ] C14 Automatic discounts, buy X get Y
- [ ] C15 Gift cards and store credit
- [x] C16 Transactional email: order confirmation, staff new-order notice, payment received (off by default), shipping confirmation with tracking, cancellation and refund emails from order events, sent once per event; shops reword any template (a small safe template language), preview it, turn templates off, set sender name and reply-to; a sent log with resend; SMTP through Zapline's mailer **alpha**
- [x] C17 Invoices and credit notes: shop legal details, an invoice per order when paid, placed or by hand, gapless numbers per series and year, lines with net, VAT and gross, totals per rate, reverse-charge note, immutable once issued; refunds issue credit notes split over the invoice's rates, staff issue more; printable HTML for staff and the buyer **alpha**
- [x] C18 PayPal and Mollie: Mollie with one platform key or organization token and a profile per method, checkout redirect, capture, cancel, refunds, webhooks that re-read the payment from Mollie; PayPal Orders v2 with buyer approval, capture or authorize, partner merchant accounts, refunds and voids, webhooks verified with PayPal; both behind features, tested against local fakes **alpha**
- [ ] C19 Carrier adapters and labels
- [ ] C20 Order edits and draft orders

## D. Storefront and content

- [x] D1 Theme engine **alpha**
- [x] D2 Default theme **alpha**
- [x] D3 Storefront checkout pages **alpha**
- [x] D4 Customer account pages **alpha**
- [x] D5 CMS pages and blocks **alpha**
- [x] D6 Menus **alpha**
- [ ] D7 Blog
- [x] D8 SEO basics: SEO title and description for products and collections, canonical and Open Graph tags, product structured data, robots.txt, sitemap.xml, noindex on private pages, URL redirects (automatic when a handle changes) **alpha**
- [x] D9 Search with facets: full-text search (prefix words over titles, SKUs, tags and descriptions, ranked), filters by tag, option value, price and collection with counts that keep each facet whole, sorting; an API and a storefront page **alpha**
- [x] D10 Translations and markets **alpha**: languages per shop, translated products, collections, pages, menus and theme texts (`/api/translations`), storefront under `/de/...` with hreflang and a switcher, German built in; markets of countries with a currency, exchange rate and rounding (`/api/markets`), price lists first, then conversion; buyers pick a country and the cart follows
- [ ] D11 Headless Storefront API
- [ ] D12 Merchant-defined content types

## E. Admin UI

- [x] E1 Admin shell **alpha**: `/admin` sign-in (HttpOnly, `SameSite=Strict` session cookie, same-origin form posts, no framing), shop picker (`GET /api/tenants`), sidebar layout with htmx; every page calls the shop API in process as the signed-in staff member
- [x] E2 Products screens **alpha**: list with live search and status filter, create, edit (price, status, tags, description, SEO), delete; variants and media screens follow
- [x] E3 Orders screens **alpha**: list with views (unpaid, to ship, open, canceled) and email search; an order's items, totals, addresses, payments (mark paid, capture), shipments (ship with tracking, delivered, cancel), refunds and cancel
- [x] E4 Customers screens **alpha**: list with search and tag filter, add, edit (details, marketing consent, tags, staff note), delete; a customer's orders and addresses
- [x] E5 Content and theme screens **alpha**: pages (blocks as JSON for now), menus written as indented lines, redirects, theme settings, theme files to edit or reset; a visual block editor follows
- [x] E6 Settings screens **alpha**
- [x] E7 Dashboard **alpha**
- [ ] E8 Command palette and shortcuts

## F. Apps and plugins

- [ ] F1 WIT interfaces v0
- [ ] F2 Plugin host in Zapline
- [ ] F3 Hook points in SOAT
- [ ] F4 App installs and consent
- [ ] F5 `soat app new` and dev loop
- [ ] F6 Admin UI extensions
- [ ] F7 Storefront blocks from apps
- [ ] F8 App directory

## G. Onboarding and import

- [x] G1 Setup wizard **alpha**
- [x] G2 Sample data and store templates **alpha**
- [x] G3 Launch checklist
- [x] G4 CSV import and export **alpha**
- [x] G5 Shopify importer **alpha** (from Shopify's product and customer CSV exports; images not yet)
- [ ] G6 Shopify delta sync and cutover
- [ ] G7 WooCommerce and Shopware importers

## H. AI layer

- [x] H1 MCP server with scoped API keys **alpha**
- [x] H2 A tool for every API operation, generated from OpenAPI **alpha**
- [x] H3 Approval mode for AI writes **alpha**
- [x] H4 Bring your own Anthropic key per shop
- [ ] H5 Shop builder from a description
- [ ] H6 Content helpers: copy, translations, alt text, SEO
- [ ] H7 Admin assistant panel

## I. Growth (after alpha)

I1 analytics · I2 abandoned cart · I3 reviews · I4 wishlists · I5 loyalty and
referrals · I6 email campaigns · I7 subscriptions · I8 B2B · I9 Google and Meta
feeds · I10 marketplaces · I11 POS

## J. Compliance (after alpha)

J1 GDPR export and delete · J2 cookie consent · J3 e-invoicing · J4 data
retention · J5 accessibility checks in CI
