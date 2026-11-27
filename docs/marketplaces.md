# Selling on marketplaces

SOAT keeps one catalog, one stock and one order list. You can sell the same
products on Amazon, eBay, Otto, Kaufland, Zalando, bol., Cdiscount, Allegro,
Etsy or any other marketplace. A **connector** is the piece that talks to the
marketplace's own API. It can be an [app](apps.md), a plugin, or a script
run on a schedule. It works through four endpoints, with an API key or app
grant that holds `marketplaces:sync`.

## Set up a marketplace

Staff do this in Admin → Marketplaces or with `marketplaces:manage`:

```http
POST /api/marketplaces
{ "name": "Amazon DE", "channel": "amazon", "currency": "EUR",
  "markup_bps": 1500, "markup_minor": 0, "stock_buffer": 1, "all_products": true }
```

- **Prices** there are the shop's prices (with price lists and markets, in
  the marketplace's currency), plus `markup_bps` basis points to cover the
  marketplace's fees, plus `markup_minor`.
- **`stock_buffer`** units are held back from what is offered, so the last
  units can't be sold twice.
- **`all_products`** offers every active product unless a listing leaves it
  out. When it is false, only listed items are offered.
- **Pausing** (`PATCH {"active": false}`) offers nothing and takes no orders.

Listings give an item its SKU or price on the marketplace, or leave it out:

```http
PUT /api/marketplaces/{id}/listings
{ "listings": [ { "sku": "MUG", "external_sku": "AMZ-MUG", "price_minor": 1490 },
                { "sku": "CAP", "listed": false } ] }
```

## The connector's loop

**1. Send offers.** The connector reads what to offer and sends it to the
marketplace:

```http
GET /api/marketplaces/{id}/offers
-> { "currency": "EUR", "items": [ { "item_id": "prod_…", "sku": "MUG",
     "external_sku": "AMZ-MUG", "title": "Mug", "price_minor": 1380,
     "compare_at_minor": null, "quantity": 4 } ] }
```

- `quantity` is what is available (on hand less what carts and orders hold)
  less the stock buffer.
- An item whose stock isn't tracked is offered with 100 units.

**2. Bring orders in.**

```http
POST /api/marketplaces/{id}/orders
{ "external_id": "302-1234567", "email": "buyer@marketplace.example",
  "shipping_address": { "first_name": "Erika", "last_name": "Muster",
    "line1": "Hauptstr. 1", "postal_code": "10115", "city": "Berlin", "country": "DE" },
  "lines": [ { "sku": "AMZ-MUG", "quantity": 2, "unit_price_minor": 1380 } ],
  "shipping_price_minor": 490 }
-> 201 { "order": { … }, "state": "open", "pending": false, … }
```

- A line names the shop's SKU, the listing's SKU there, or an `item_id`.
- The order is placed at the marketplace's prices and holds stock.
- It is recorded as paid, with payment provider `marketplace`, unless
  `"paid": false` is sent.
- Sending the same `external_id` again answers `200` with the first order,
  so the connector can retry safely.
- Errors:
  - an unknown SKU is `422`;
  - too little stock is `409 insufficient_stock`;
  - a paused marketplace is `409 marketplace_paused`.

**3. Report back.** Staff ship the order in SOAT as usual. The connector
then asks what the marketplace hasn't been told yet:

```http
GET /api/marketplaces/{id}/orders?pending=true
-> { "items": [ { "external_id": "302-1234567", "state": "shipped",
     "synced_state": "open", "pending": true,
     "fulfillments": [ { "carrier": "DHL", "tracking_number": "00340434", … } ] } ] }
```

It confirms the shipment to the marketplace with the tracking numbers, then
marks the order as told:

```http
POST /api/marketplaces/{id}/orders/302-1234567/synced
{ "state": "shipped" }
```

`state` is one of `open`, `partially_shipped`, `shipped` or `canceled`.

**4. Cancellations.** When the marketplace cancels an order, the connector
cancels it in SOAT. Its stock is released, and a shipped order answers
`409 not_cancelable`. Refunds of marketplace payments happen on the
marketplace.

```http
POST /api/marketplaces/{id}/orders/302-5/cancel
{ "reason": "buyer asked" }
```

## Events

Webhooks and subscribers can react to these events:

- `marketplace.created`, `marketplace.updated`, `marketplace.deleted`
- `marketplace.listings_updated`
- `marketplace_order.imported`, `marketplace_order.synced`
- the usual `order.*` events of each order

The rules for prices, quantities and states are in `soat_core::marketplaces`.
