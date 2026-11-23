# SOAT roadmap

The working copy of the micro-plan: one line per milestone, one milestone per
PR. Tick a line in the PR that finishes it. The reasoning, the full feature
catalogue and open questions are in the plan doc
(https://claude.ai/artifact/2Rzd1CxY5h8vJygFPXjNeY).

Lines marked **alpha** are in the public alpha (week of 2027-01-04).

## Layers

Each layer may use only the layers below it. Siblings in one layer talk
through events, never by importing each other.

| Layer | Crates (created when their milestone starts) |
| --- | --- |
| Surfaces | `soat-storefront`, `soat-admin`, REST API, `soat-mcp` |
| Extensions (Cargo features) | `soat-apps`, `soat-plugin-api`, `soat-plugin-host`, `soat-cli`, `soat-onboarding`, `soat-import`, `soat-ai`, `soat-analytics`, `soat-marketing`, `soat-subscriptions`, `soat-b2b`, `soat-channels`, `soat-compliance` |
| Content | `soat-cms`, `soat-i18n`, `soat-seo`, `soat-search`, `soat-theme` |
| Selling | `soat-customers`, `soat-cart`, `soat-tax`, `soat-shipping`, `soat-payments`, `soat-checkout`, `soat-orders`, `soat-promotions`, `soat-notify` |
| Catalog | `soat-catalog`, `soat-fields`, `soat-inventory`, `soat-pricing`, `soat-files` |
| Foundation | `soat-core`, `soat-module`, `soat-tenancy`, `soat-db`, `soat-events`, `soat-auth`, `soat-webhooks` |
| Zapline + Postgres | gains tenancy, outbox, webhooks, plugin host and MCP as SOAT needs them |

## A. Foundation

- [x] A1 Module system: `soat-module`, tenancy and catalog as module crates, per-module migrations **alpha**
- [x] A2 Typed IDs (`prod_…`) and list endpoints (Zapline `Listing`: offset pages, sort, search, filters) **alpha**; cursor pagination later as a Zapline addition
- [ ] A3 Postgres as main database, SQLite for fast tests, CI runs both **alpha**
- [ ] A4 Row-level security per shop, isolation tests on Postgres **alpha**
- [ ] A5 Platform admin bootstrap, staff accounts, login; tenant creation behind it **alpha**
- [ ] A6 Roles and permissions **alpha**
- [ ] A7 API keys with scopes **alpha**
- [ ] A8 Domain events and transactional outbox **alpha**
- [ ] A9 Webhooks: signed, retried, logged **alpha**
- [ ] A10 Audit log **alpha**
- [ ] A11 Shop resolution by domain and API key **alpha**
- [ ] A12 CI check for forbidden crate dependencies

## B. Catalog

- [ ] B1 Variants and options **alpha**
- [ ] B2 Collections **alpha**
- [ ] B3 Smart collections
- [ ] B4 Categories and tags **alpha**
- [ ] B5 Custom fields **alpha**
- [ ] B6 Files and media **alpha**
- [ ] B7 Image pipeline
- [ ] B8 Inventory and locations **alpha**
- [ ] B9 Stock reservations **alpha**
- [ ] B10 Price lists and currencies **alpha**
- [ ] B11 Group and tier prices
- [ ] B12 Digital products and bundles

## C. Selling

- [ ] C1 Customers **alpha**
- [ ] C2 Customer login **alpha**
- [ ] C3 Cart **alpha**
- [ ] C4 Tax zones and rates, EU VAT **alpha**
- [ ] C5 Tax provider trait
- [ ] C6 Shipping zones and rates **alpha**
- [ ] C7 Payments core and manual payment **alpha**
- [ ] C8 Stripe **alpha**
- [ ] C9 Checkout **alpha**
- [ ] C10 Orders **alpha**
- [ ] C11 Fulfillment **alpha**
- [ ] C12 Refunds and returns **alpha**
- [ ] C13 Discount codes **alpha**
- [ ] C14 Automatic discounts, buy X get Y
- [ ] C15 Gift cards and store credit
- [ ] C16 Transactional email **alpha**
- [ ] C17 Invoices and credit notes **alpha**
- [ ] C18 PayPal and Mollie
- [ ] C19 Carrier adapters and labels
- [ ] C20 Order edits and draft orders

## D. Storefront and content

- [ ] D1 Theme engine **alpha**
- [ ] D2 Default theme **alpha**
- [ ] D3 Storefront checkout pages **alpha**
- [ ] D4 Customer account pages **alpha**
- [ ] D5 CMS pages and blocks **alpha**
- [ ] D6 Menus **alpha**
- [ ] D7 Blog
- [ ] D8 SEO basics **alpha**
- [ ] D9 Search with facets **alpha**
- [ ] D10 Translations and markets **alpha**
- [ ] D11 Headless Storefront API
- [ ] D12 Merchant-defined content types

## E. Admin UI

- [ ] E1 Admin shell **alpha**
- [ ] E2 Products screens **alpha**
- [ ] E3 Orders screens **alpha**
- [ ] E4 Customers screens **alpha**
- [ ] E5 Content and theme screens **alpha**
- [ ] E6 Settings screens **alpha**
- [ ] E7 Dashboard **alpha**
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

- [ ] G1 Setup wizard **alpha**
- [ ] G2 Sample data and store templates **alpha**
- [ ] G3 Launch checklist
- [ ] G4 CSV import and export **alpha**
- [ ] G5 Shopify importer **alpha**
- [ ] G6 Shopify delta sync and cutover
- [ ] G7 WooCommerce and Shopware importers

## H. AI layer

- [ ] H1 MCP server with scoped API keys **alpha**
- [ ] H2 A tool for every API operation, generated from OpenAPI **alpha**
- [ ] H3 Approval mode for AI writes **alpha**
- [ ] H4 Bring your own Anthropic key per shop
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
