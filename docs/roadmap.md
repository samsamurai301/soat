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
| Selling | `soat-customers`, `soat-cart`, `soat-tax`, `soat-shipping`, `soat-payments`, `soat-checkout`, `soat-orders`, `soat-promotions`, `soat-giftcards`, `soat-notify` |
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
- [x] B3 Smart collections: rules on tag, title, SKU, tax class, price or weight (all or any), membership refilled when rules change and kept as products are created or changed; hand-picked edits refused (409 `smart_collection`); admin collection screens for both kinds
- [x] B4 Categories (a tree, up to 8 deep, listing includes subtrees) and product tags (`?tag=`) **alpha**
- [x] B5 Custom fields: `soat-fields` (foundation), typed definitions per resource type, product values at `/api/products/{id}/fields` **alpha**
- [x] B6 Files and media: `soat-files` (foundation) on Zapline storage, type from content (no HTML/SVG), signed links, ordered product media **alpha**
- [x] B7 Image pipeline: JPEG and PNG uploads get smaller copies at 320, 640, 1280 and 1920 px wide (never upscaled), with decoding bounded in size; the storefront serves them as `srcset` with `width`/`height`, and `POST /api/files/{id}/sizes` makes them again
- [x] B8 Inventory and locations: stock per variant (or variant-less product) and location, adjustments with reasons, counts, transfers, a ledger **alpha**
- [x] B9 Stock reservations: by reference (cart or order), optional expiry, locked per item so nothing oversells **alpha**
- [x] B10 Price lists and currencies: lists per currency with priority, compare-at prices, `GET /api/prices/{item}?currency=` resolution **alpha**
- [x] B11 Group and tier prices: a price list can be for one customer tag (a group); prices can start at a quantity (tiers); carts and draft orders price lines for the customer and quantity
- [x] B12 Digital products and bundles: digital products ship nothing, are untracked by default, are delivered on payment and open their downloads (signed links) to the buyer; bundles sell a set of items from their stock (availability, holds, shipping and restocking go to the items)

## C. Selling

- [x] C1 Customers: profiles, unique email per shop, marketing consent with time, tags, addresses with one default, custom fields **alpha**
- [x] C2 Customer login: storefront register, sign in, sign out, own profile; `Shop` extractor for public requests; claiming a known email by an emailed link (see C21) **alpha**
- [x] C3 Cart: token-held storefront carts, live prices and stock per line, customers claim carts, staff list them **alpha**
- [x] C4 Tax zones and rates, EU VAT: zones per country set, rates per tax class in basis points, products carry a class, EU preset, origin or destination (OSS) VAT, reverse charge, `POST /api/tax/quote` **alpha**
- [x] C5 Tax provider trait: `TaxProvider` adapters registered on the tax module, the built-in `http` adapter (signed JSON to the shop's tax service); a shop picks one with sealed credentials (`/api/tax/provider`), answers are kept 10 minutes, and the zones stand in when it fails, saying why (guide: `docs/tax-providers.md`)
- [x] C6 Shipping zones and rates: zones of countries, rates per currency with inclusive value and weight bounds, delivery estimates, product weights, `POST /api/shipping/options` **alpha**
- [x] C7 Payments core and manual payment: `PaymentProvider` trait registered on the module, payment methods per shop, payments with a status machine, capture, cancel, refunds, manual provider with instructions and mark-paid **alpha**
- [x] C8 Stripe: PaymentIntents with one platform key and optional Connect account per method, client secret handed to the buyer once and never stored, manual capture, cancel, refunds, signed webhooks checked against the stored intent; behind the `stripe` feature **alpha**
- [x] C9 Checkout: email, addresses, VAT number and shipping choice per cart, live totals with tax and what is missing, completion places the order, holds stock (an hour for card payments, until paid otherwise), closes the cart and starts the payment **alpha**
- [x] C10 Orders: `place()` copies lines, tax, shipping and addresses in, per-shop numbers from 1001, financial status follows payments through a subscriber, cancel releases held stock, guest token and customer account views **alpha**
- [x] C11 Fulfillment: ship some or all of an order's lines from a chosen or best-stocked location (recorded as `sold`, holds released), tracking, delivered, cancel a shipment to restock, order fulfillment status **alpha**
- [x] C12 Refunds and returns: customers or staff request returns of shipped units, staff receive (restock where they shipped from, or not), reject, or refund them through the order's payments with optional shipping; goodwill refunds of any amount across payments **alpha**
- [x] C13 Discount codes: percentage, fixed amount or free shipping, with currency, minimum subtotal, start and end dates, a usage limit and once per customer; entered at checkout, shared over the lines before tax, recorded on the order and redeemed with it **alpha**
- [x] C14 Automatic discounts and buy X get Y: discounts without a code that apply by themselves (the one worth most, before any code), buy X get Y with a share off the cheapest units of every lot, any discount limited to a collection's products; a discounts screen in the admin **alpha**
- [x] C15 Gift cards and store credit: staff issue gift cards (the code shown once, only a hash kept) and customers' store credit, adjust and disable them, with a ledger; buyers check a balance, spend a gift card at checkout and signed-in customers their store credit, recorded as paid payments of the order so refunds put the money back; admin screens **alpha**
- [x] C16 Transactional email: order confirmation, staff new-order notice, payment received (off by default), shipping confirmation with tracking, cancellation and refund emails from order events, sent once per event; shops reword any template (a small safe template language), preview it, turn templates off, set sender name and reply-to; a sent log with resend; SMTP through Zapline's mailer **alpha**
- [x] C17 Invoices and credit notes: shop legal details, an invoice per order when paid, placed or by hand, gapless numbers per series and year, lines with net, VAT and gross, totals per rate, reverse-charge note, immutable once issued; refunds issue credit notes split over the invoice's rates, staff issue more; printable HTML for staff and the buyer **alpha**
- [x] C18 PayPal and Mollie: Mollie with one platform key or organization token and a profile per method, checkout redirect, capture, cancel, refunds, webhooks that re-read the payment from Mollie; PayPal Orders v2 with buyer approval, capture or authorize, partner merchant accounts, refunds and voids, webhooks verified with PayPal; both behind features, tested against local fakes **alpha**
- [x] C19 Carrier adapters and labels: a carrier adapter trait; shops add carrier accounts whose credentials are sealed and never shown again; labels printed per shipment put their tracking on it and keep the PDF as a file, and can be voided; Sendcloud (DHL, DPD, GLS, PostNL, UPS and more) behind a feature, tested against a local fake; print from the admin order page
- [x] C20 Order edits and draft orders: staff draft an order with catalog items at their prices or their own, shipping, a discount and a note, see totals with tax, and place it as paid or with a payment started; open orders change quantities, lose unshipped lines and gain items, with stock holds, totals and financial status following and the balance owed either way **alpha**
- [x] C21 Password reset and account activation: a customer asks for a link (`POST /api/storefront/account/access`, the same answer for every email, three an hour) and sets a password with it (`.../access/complete`), which signs them in and ends their other sessions; a guest's email is activated the same way; the link goes to the shop's domain or comes as a code; the token is never in an event and the email's body is not kept once sent; storefront pages `/account/forgot` and `/account/access`; reworded like any email
- [x] C22 Gift cards sold as products: a product tagged `gift-card` issues one card per unit at the line's price when its order is paid, and the buyer gets each code by email once, and refunding the whole order turns them off; the code is in no event and not kept in the email log, which cannot resend it **alpha**

## D. Storefront and content

- [x] D1 Theme engine **alpha**
- [x] D2 Default theme **alpha**
- [x] D3 Storefront checkout pages **alpha**
- [x] D4 Customer account pages **alpha**
- [x] D5 CMS pages and blocks **alpha**
- [x] D6 Menus **alpha**
- [x] D7 Blog: posts in Markdown with excerpt, image, author and tags, drafts, scheduled and published; `/blog` with tags and pages, `/blog/{handle}`, an Atom feed and sitemap entries; storefront API; admin screens **alpha**
- [x] D8 SEO basics: SEO title and description for products and collections, canonical and Open Graph tags, product structured data, robots.txt, sitemap.xml, noindex on private pages, URL redirects (automatic when a handle changes) **alpha**
- [x] D9 Search with facets: full-text search (prefix words over titles, SKUs, tags and descriptions, ranked), filters by tag, option value, price and collection with counts that keep each facet whole, sorting; an API and a storefront page **alpha**
- [x] D10 Translations and markets **alpha**: languages per shop, translated products, collections, pages, menus and theme texts (`/api/translations`), storefront under `/de/...` with hreflang and a switcher, German built in; markets of countries with a currency, exchange rate and rounding (`/api/markets`), price lists first, then conversion; buyers pick a country and the cart follows
- [x] D11 Headless Storefront API: the storefront's catalog, pages and menus as JSON in the buyer's language (`?lang=` or `/de/`) and market (`?country=`), the shop's profile (`/api/storefront/shop`), a collection list, any-origin CORS on `/api/storefront/` without cookies; guide in `docs/headless.md`
- [x] D12 Merchant-defined content types: a shop defines types (`PUT /api/content-types/{key}`, up to 50 typed fields with the custom-field kinds, required or not) and adds entries (`/api/content/{type}`), drafts or published; changing a type drops removed fields and refuses changes entries would not fit; public types' published entries are on the storefront API (`/api/storefront/content/{type}`). Admin screens and translations of entries to follow

## E. Admin UI

- [x] E1 Admin shell **alpha**: `/admin` sign-in (HttpOnly, `SameSite=Strict` session cookie, same-origin form posts, no framing), shop picker (`GET /api/tenants`), sidebar layout with htmx; every page calls the shop API in process as the signed-in staff member
- [x] E2 Products screens **alpha**: list with live search and status filter, create, edit (price, status, tags, description, SEO), delete; options written as lines, variants added one by one or all missing at once, priced and deleted; images and video uploaded, described (alt text), ordered and removed
- [x] E3 Orders screens **alpha**: list with views (unpaid, to ship, open, canceled) and email search; an order's items, totals, addresses, payments (mark paid, capture), shipments (ship with tracking, delivered, cancel), refunds and cancel
- [x] E4 Customers screens **alpha**: list with search and tag filter, add, edit (details, marketing consent, tags, staff note), delete; a customer's orders and addresses
- [x] E5 Content and theme screens **alpha**: pages (blocks as JSON for now), menus written as indented lines, redirects, theme settings, theme files to edit or reset; a visual block editor follows
- [x] E6 Settings screens **alpha**
- [x] E7 Dashboard **alpha**
- [x] E8 Command palette and shortcuts: Ctrl+K, Cmd+K or `/` finds pages, products, orders (by number or email) and customers through the API as the staff member; `g` then a letter goes to home, orders, products, customers or settings

## F. Apps and plugins

- [x] F1 WIT interfaces v0: the `soat:plugin@0.1.0` package (worlds `discounts`, `shipping-filter`, `checkout-validation`, `events`, and the `host` interface), its records mirroring `soat_core::hooks` with the bounds SOAT holds plugins to, and the `soat.plugin.json` manifest (crate `soat-plugin-api`, guide `docs/plugins.md`)
- [x] F2 Plugin host: `soat-plugin-host` runs `soat:plugin` components on wasmtime as `Hook`s, a fresh instance per call with fuel (50M) and memory (32 MiB) caps and only the `host` interface; failures change nothing and are logged; `SOAT_PLUGINS_DIR` loads them at start (feature `plugins`). Built in SOAT first; the generic engine moves into Zapline once it settles
- [x] F3 Hook points in SOAT: `soat_core::hooks::Hook` runs at the storefront checkout: shipping options filtered (hidden or cheaper only), amounts taken off after SOAT's discounts (kept within the cart, cart amounts spread over lines), orders blocked with a reason (`order_blocked`); native hooks come in `Settings::hooks`, plugins through the same trait
- [x] F4 App installs and consent: developers register apps (`/api/apps`, client id and secret, https redirect URIs, declared permissions); an owner approves on the consent screen (`/admin/{shop}/apps/authorize`), the app trades a single-use code for an API key holding exactly the granted permissions in that shop, and uninstalling revokes the key at once (crate `soat-apps`, feature `apps`)
- [x] F5 `soat app new` and dev loop: `soat login`, `soat app new` (a Node app with no packages: install flow, an admin link, a storefront block), `soat app push` and `soat app dev --shop` register or update the app from its `soat.app.json`, keep its secret in `.env` and print the install link (crate `soat-cli`, commands of the `soat` binary)
- [x] F6 Admin UI extensions: an app declares up to 10 admin links (`admin_links` on `PATCH /api/apps/{id}`) on the Apps page or next to a product, order or customer; once installed and connected they show there (`GET /api/app-links`, `apps:read`) and open the app with the shop and the record's id
- [x] F7 Storefront blocks from apps: an app offers up to 10 blocks (`storefront_blocks`: a script it hosts, on every page or on product pages); the owner turns each on per install (`PUT /api/app-installations/{id}/blocks`, or the Apps page), and the default theme loads the script and leaves a `.soat-app-block` element for it (guide: `docs/apps.md`)
- [x] F8 App directory: a developer lists an app (`PATCH /api/apps/{id}` with tagline, description, category, `app_url` and `listed`); shops search listed apps at `GET /api/app-directory` and `/admin/{shop}/apps/directory`, and installing starts at the app's `app_url?shop=…`, which sends the owner to the consent screen
- [x] F9 Plugins per shop: plugins run only for shops that turn them on (`GET /api/plugins`, `PUT /api/plugins/{name}`, `plugins:read` and `plugins:manage`), with each shop's settings over the install's, secret ones sealed; admin Settings, Plugins
- [x] F10 A shop's own plugins: a shop uploads a plugin (manifest and component, `POST /api/plugins/uploads`, up to 8 MiB, checkout worlds only) that runs for it alone, after the install's, once turned on; replace by uploading again, remove with `DELETE /api/plugins/uploads/{name}`; compiled once per manifest and component; admin Settings, Plugins

## G. Onboarding and import

- [x] G1 Setup wizard **alpha**
- [x] G2 Sample data and store templates **alpha**
- [x] G3 Launch checklist
- [x] G4 CSV import and export **alpha**
- [x] G5 Shopify importer **alpha** (from Shopify's product and customer CSV exports; images not yet)
- [x] G6 Shopify delta sync and cutover: a shop connects its store (`*.myshopify.com` and an Admin API token, sealed) at `/api/imports/shopify`; each sync pulls the products and customers changed since the last one as SOAT's import file and the admin imports them through the API; the last sync cuts over and later pulls are refused (crate `soat-import`, feature `shopify-sync`)
- [x] G7 WooCommerce and Shopware importers: WooCommerce's product export (simple and variable products, variations as variants) and Shopware 6's product and customer exports (default profiles; variants under one `Variant` option) import as they are on the admin's import screen, prices and weights normalized; images not yet

## H. AI layer

- [x] H1 MCP server with scoped API keys **alpha**
- [x] H2 A tool for every API operation, generated from OpenAPI **alpha**
- [x] H3 Approval mode for AI writes **alpha**
- [x] H4 Bring your own Anthropic key per shop
- [x] H5 Shop builder from a description: Claude drafts products (with variants), collections and pages as structured output (`POST /api/assistant/plan`), the owner reviews the draft at `/admin/{shop}/build` and builds it through the API
- [x] H6 Content helpers: Claude suggests a product's or collection's description and SEO fields, or translates them (`POST /api/assistant/write`); the product screen shows a suggestion to review, or saves a translation into one of the shop's languages. Alt text waits for alt text on media (B7)
- [x] H7 Admin assistant panel: chats with Claude in the admin on the shop's own key, every API tool found by tool search and run as the staff member, changes approved in the chat

## I. Growth (after alpha)

- [x] I1 Analytics (feature `analytics`): storefront visits counted without cookies or stored addresses (a visitor is a hash of the day's salt, the shop, the address and the browser, and the salt is deleted the next day), bots, API calls and error pages left out; `GET /api/analytics?days=` (`analytics:read`) reports visitors, views and visitors per day, the top pages, sources (`utm_source` or the referring site) and devices, the way from visit to cart, order and payment with the conversion rate, and first versus repeat orders; admin Analytics with a chart; views older than 400 days are deleted
- [x] I2 Abandoned cart emails: a shop turns them on with a delay (15 minutes to 7 days, `PUT /api/cart-recovery`, or admin Settings, Email); a scan every 5 minutes finds open carts with an email and lines left that long (and under a week) and publishes `cart.abandoned`, once per cart; the buyer gets one `cart_recovery` email with a link (`/cart/recover?token=`, or `POST /api/storefront/carts/recover`) that brings the cart back under a new token; the link is in no event and the email's body is not kept
- [x] I3 Product reviews (feature `reviews`): a signed-in customer rates a product 1 to 5 with a title and text (`PUT /api/storefront/products/{id}/review`, or the product page's form), once per product, marked verified when they bought it in a paid order; staff publish, reject, reply and delete (`/api/reviews`, `reviews:read` and `reviews:manage`, admin Reviews); the product page shows published reviews with the average rating and `aggregateRating` in its structured data (`GET /api/storefront/products/{id}/reviews`)
- [x] I4 Wishlists (feature `wishlists`): a signed-in customer saves products for later from the product page or the API (`PUT`/`DELETE /api/storefront/account/wishlist/{product_id}`, up to 200) and sees them, as they are today, under Account → Wishlist (`GET /api/storefront/account/wishlist`); staff see the most saved products (`GET /api/wishlists/most-saved`, `wishlists:read`); `wishlist.item_added` and `wishlist.item_removed` events
- [x] I5 Loyalty points and referrals (feature `loyalty`): a shop turns its program on and sets its terms (`/api/loyalty`, `loyalty:read` and `loyalty:manage`, admin Gift cards); customers earn points for the goods of paid orders, lose them when an order is refunded in full, and turn them into store credit spent at checkout (`POST /api/storefront/account/loyalty/redeem`, Account → Points); each customer has a referral code and link (`/account/register?ref=`), and a friend who enters it before their first paid order gets points with it, as does the referrer; staff see and adjust a customer's points (`/api/loyalty/customers/{id}`, the customer page); `loyalty.points_changed`, `loyalty.referral_entered` and `loyalty.updated` events
- [x] I6 Email campaigns (feature `marketing`): staff write a campaign (subject, text, optional HTML with `first_name`, `shop` and `unsubscribe_url`), send themselves a test and send it to the customers who agreed to news, optionally only those with a tag (`/api/campaigns`, `marketing:read` and `marketing:manage`, admin Campaigns); a background job mails them in batches; every email has a one-click unsubscribe link (and `List-Unsubscribe` headers) whose token is kept only as a hash; the link's page asks before it unsubscribes (`/unsubscribe`, `POST /api/storefront/unsubscribe`); results show sent, failed and unsubscribed
- [x] I7 Subscriptions (feature `subscriptions`): staff give a product plans, every N weeks or months with an optional discount and shipping price (`/api/subscription-plans`, `subscriptions:read` and `subscriptions:manage`, admin Subscriptions); a signed-in customer subscribes from the product page or the API (`POST /api/storefront/account/subscriptions`), which places the first order at once to their address; a job places each renewal as a completed order with the plan's discount, moving the next date before it orders so a crash never orders twice, and a failed renewal is noted and tried again the next day; customers (Account → Subscriptions) and staff pause, resume, skip the next order and cancel; `subscription.created`, `.paused`, `.resumed`, `.skipped`, `.canceled`, `.renewed` and `.renewal_failed` events
- [x] I8 B2B (feature `b2b`): companies group the customers who buy for a business (`/api/companies`, `companies:read` and `companies:manage`, admin Companies); members get the company's price tag, so price lists for that customer group apply to them; a company has a VAT number, payment terms (orders due N days after they are placed), a credit limit and can be put on hold; a ledger enters every member's order and gives what the company owes, what is overdue and the credit left (`/api/companies/{id}/balance`, Account → Company for its admins); buyers ask for quotes (`POST /api/storefront/account/quotes`, Account → Quotes), staff answer with an offer that is a draft order at their prices (`POST /api/quotes/{id}/offer`), and the buyer accepts it within its days (the order is placed, due within the terms; over the credit limit or on hold is refused) or declines; buyers quick order by SKU into the cart or a quote (Account → Quick order); `company.*` and `quote.*` events
- [x] I9 Google and Meta product feeds (feature `feeds`, crate `soat-channels`): staff make feeds (`/api/feeds`, `channels:read` and `channels:manage`, admin Products → Feeds) for Google Merchant Center (RSS 2.0 with `g:`) or a Meta catalog (CSV), in a currency, optionally for one collection and without sold-out items; each has a private URL (`/feeds/{token}`, renewable) that the channel fetches on its schedule; one item per product or variant (grouped by product) with the price buyers pay (a compare-at price becomes the regular price, the lower one the sale price), stock, pictures, brand, GTIN, MPN (custom fields `brand`, `gtin`, `mpn`, else the shop's name and SKU) and weight; the preview (`/api/feeds/{id}/preview`) names what the channel would reject (no picture, no description, a wrong GTIN check digit) or show worse; feed rendering and GTIN checks are in `soat_core::feeds`
- [x] I10 Marketplaces (feature `marketplaces`, crate `soat-marketplaces`, guide `docs/marketplaces.md`): staff set up marketplaces (`/api/marketplaces`, `marketplaces:read` and `marketplaces:manage`, admin Marketplaces) for Amazon, eBay, Otto, Kaufland, Zalando, bol., Cdiscount, Allegro, Etsy or others, each in a currency with a markup on the shop's prices, units held back from the stock offered, and every product or only listed ones; listings give an item its SKU or price there or leave it out; a connector with `marketplaces:sync` reads the offers (SKU, price, quantity available), brings orders in (placed at the marketplace's prices, paid, holding stock, once per marketplace order number, by the shop's or the marketplace's SKU), reads what the marketplace has not been told (shipped with tracking, canceled) and marks it told, and cancels orders the marketplace canceled; the rules are in `soat_core::marketplaces`
- [x] I11 Point of sale (feature `pos`, crate `soat-pos`): registers at a location (`/api/pos/registers`, `pos:manage`); staff open a session with the cash in the drawer (one open per register), ring up sales by SKU or item (`/api/pos/sessions/{id}/sales`, `pos:sell`) with a cashier's price or discount, paid in cash (with change), card or other tenders; each sale is an order placed, paid and handed over at once, with stock taken at the register's location and taxes at its address; the quote works out a total without keeping anything; the session report gives sales and totals by tender, the cash expected in the drawer and the difference to what was counted at closing; admin Point of sale has the till screen (type or scan SKUs, total, take payment, all by card) and printable receipts

## J. Compliance (after alpha)

- [x] J1 GDPR export and erasure (feature `privacy`, crate `soat-compliance`, guide `docs/privacy.md`): every module declares the tables in which it keeps customer data (`Module::personal_data`: how rows match a customer, what an export shows, and whether erasing deletes, anonymizes or keeps them for a legal reason); staff download a customer's data as JSON (`privacy:export`) and erase them after typing their email again (`privacy:erase`: account, addresses, carts, reviews, points and the like deleted, orders and discount uses kept without them, invoices kept for tax law, the event log redacted, with a report per table); customers download their own data and ask to be erased from their account, and staff see and decline requests (admin Privacy requests); a test fails for any table with customer columns and no declaration

- [x] J2 Cookie consent (feature `privacy`, guide `docs/privacy.md`): htmx is served by SOAT itself (`soat_core::assets`), so neither storefront nor admin contacts a third party; a shop's cookie banner (`/api/consent`, `privacy:settings`, admin Privacy) with Accept all and Reject all side by side and a choice per purpose (analytics, marketing, apps), kept six months and asked again for a new version; Google Analytics 4 (with Consent Mode v2), the Meta Pixel and apps' scripts load only with consent to their purpose; each choice recorded without who made it, with stats; `/consent` settings page linked from the footer; headless storefronts read the settings and record choices (`/api/storefront/consent`)

- [x] J3 E-invoices (crate `soat-invoices`, logic in `soat_core::einvoice`, guide `docs/e-invoices.md`): every invoice and credit note downloads as an EN 16931 e-invoice in XRechnung 3.0 (CII), the XML of ZUGFeRD / Factur-X (profile EN 16931) or Peppol BIS Billing 3.0 (UBL), for staff and for the buyer holding the order's token, with the buyer's reference (Leitweg-ID), VAT categories S, Z, AE (reverse charge) and G (export), the IBAN from the bank details and what was paid; a check names by rule what a receiver's validator would reject (seller phone and email for XRechnung, VAT IDs for reverse charge, totals that do not add up), shown on the admin order page beside the downloads

- [x] J4 Data retention (feature `privacy`, guide `docs/privacy.md`): modules declare how long their old rows are kept (`Module::retention`: carts never checked out 90 days, expired sign-ins 30, sent emails 730, cookie choices 1095, the event log and closed privacy requests kept by default), shops change the days within each rule's minimum or keep the data (`/api/retention`, admin Privacy), and can erase customers with no order or sign-in for a time like by hand; an hourly task sweeps each shop once a day, and staff see what is due and sweep now; orders and invoices are never removed

- [x] J5 Accessibility checks (guide `docs/accessibility.md`): `soat_testing::a11y` checks rendered HTML against the WCAG 2.1 criteria markup alone can break (image alt, field labels, link and button names, heading order, duplicate ids, lang, title, positive tabindex, zoom-blocking viewport, frame titles); a test crawls every admin, storefront, account, cart and checkout page reachable from a sample shop and fails on any issue, in `cargo test` and so in CI
