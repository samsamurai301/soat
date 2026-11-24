# Headless storefronts

SOAT renders its own storefront, but every page of it is also an API under
`/api/storefront/`, so a shop can build its front end elsewhere (Next.js,
Astro, a mobile app) and keep SOAT for the catalog, carts, checkout,
payments and orders.

## Calling the API

- **Shop:** send `x-soat-tenant: <shop id>`, or call SOAT on one of the
  shop's domains.
- **Language:** `?lang=de`, or a prefix (`/de/api/storefront/...`). A
  language the shop does not publish falls back to its default.
- **Market:** `?country=CH` shows prices in that market's currency. A
  country outside the shop's markets keeps the shop's own prices.
- **Browsers:** every `/api/storefront/` endpoint answers any origin
  (`Access-Control-Allow-Origin: *`). No cookies are sent: customers use a
  bearer token, carts and orders their own tokens. The admin API stays
  same-origin.

The full reference, with every field, is at `/docs` under the
`storefront` tag.

## A shop's pages

| What | Endpoint |
| --- | --- |
| Name, languages, currency | `GET /api/storefront/shop` |
| Products (filters, sort, pages) | `GET /api/storefront/products` |
| Search with facets | `GET /api/storefront/search` |
| One product with variants | `GET /api/storefront/products/{handle}` |
| Collections | `GET /api/storefront/collections`, `/collections/{handle}` |
| Content pages and menus | `GET /api/storefront/pages/{handle}`, `/menus/{handle}` |
| Blog | `GET /api/storefront/blog`, `/blog/{handle}` |
| Markets | `GET /api/storefront/markets?country=CH` |

Product images carry `srcset`, `width` and `height` for responsive `<img>`
tags.

## Buying

1. `POST /api/storefront/carts` starts a cart and returns its `token`
   once. Send it as `x-soat-cart-token` on every call for that cart.
2. `POST /api/storefront/carts/{id}/lines` adds items; `PATCH` and
   `DELETE` on `/lines/{line_id}` change them.
3. `GET /api/storefront/carts/{id}/checkout` shows totals with shipping and
   tax, and what is still `missing`. `PATCH` the same path with the email,
   addresses, shipping choice or discount code.
4. `GET /api/storefront/payment-methods` lists how the buyer can pay;
   `POST /api/storefront/carts/{id}/checkout/complete` places the order
   and returns it with an `order_token`.
5. `GET /api/storefront/orders/{id}` with `x-soat-order-token` shows the
   order, its invoices and downloads to a guest.

## Customer accounts

`POST /api/storefront/account/register` and `/account/login` return a
`soat_ct_...` token; send it as `Authorization: Bearer <token>` to
`/api/storefront/account`, `/account/orders` and
`POST /api/storefront/carts/{id}/customer` (to attach a cart).
