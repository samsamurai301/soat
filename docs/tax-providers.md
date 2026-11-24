# Tax providers

By default a shop's tax comes from its own zones and rates (EU VAT is a
preset). A shop whose rules go past a rate per country (US sales tax by
address, many registrations, an accountant's own service) can let a tax
service work it out instead.

## Pick a provider

```http
GET /api/tax/providers            -> [{ "name": "http", "label": "Tax service (HTTP)" }]
PUT /api/tax/provider             (tax:manage)
{ "provider": "http",
  "settings": { "url": "https://tax.example.com/quote" },
  "credentials": { "secret": "at least 16 characters" } }
DELETE /api/tax/provider          back to the zones
```

The credentials are sealed with the install's `SOAT_SECRET_KEY` and never
shown again. Events `tax.provider_set` and `tax.provider_removed` carry
no secrets.

## The `http` adapter's request

SOAT POSTs JSON to `url`, signed like webhooks: the `soat-signature`
header is `t=<unix seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>">` with
the secret.

```json
{ "origin_country": "US", "country": "US", "postal_code": "10001",
  "vat_number": null, "prices_include_tax": false,
  "lines": [{ "tax_class": "standard", "amount_minor": 10000 },
            { "tax_class": "shipping", "amount_minor": 500 }] }
```

Amounts are line totals after discounts, in minor units, gross when
`prices_include_tax` is true. Answer with the tax on each line, in order:

```json
{ "lines": [{ "rate_bps": 888, "tax_minor": 888 }, { "rate_bps": 888, "tax_minor": 44 }] }
```

`rate_bps` is basis points (888 is 8.88%).

## What SOAT does with it

- The answer is kept for 10 minutes, so checkout pages showing the same
  cart do not ask again.
- If the service cannot be reached, answers with an error, or answers with
  tax that cannot be right (a negative amount, more than the line, or the
  wrong number of lines), the zones work out the tax. The quote then has
  `"source": "zones"` and says why in `provider_error`. Otherwise `source`
  names the provider.
- Checkout, draft orders and order edits all use the provider.

## Other adapters

An adapter implements `soat_tax::providers::TaxProvider` and is registered
with `Taxes::new().with(adapter)`.
