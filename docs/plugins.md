# Plugins (v0 interfaces)

Apps run on their own servers and use the API. Plugins run inside SOAT,
as WebAssembly components, at the hook points where SOAT works out a cart
or places an order. They are small, fast and sandboxed: no network, no
files, no clock, only what the `host` interface lends.

The interfaces are the WIT package `soat:plugin@0.1.0`, in
`crates/soat-plugin-api/wit/soat-plugin.wit`. A plugin targets one world:

| World | Export | SOAT keeps it to |
| --- | --- | --- |
| `discounts` | `discount(cart) -> list<adjustment>` | amounts that are not negative, on lines that exist, never more than is left of a line or the cart |
| `shipping-filter` | `filter(cart, options) -> list<options>` | options SOAT offered, in its order, at no more than its price |
| `checkout-validation` | `validate(cart) -> verdict` | `allow`, or `block(reason)` shown to the buyer |
| `events` | `on-event(event) -> result` | reacting; it changes nothing it is given |

Every world imports `host`: `log(level, message)` and `setting(key)`.

The records (`hook-cart`, `hook-line`, `adjustment`, `hook-shipping-option`,
`hook-event`, `verdict`) mirror `soat_core::hooks`; amounts are integer
minor units of the cart's currency.

## The manifest

A plugin ships with `soat.plugin.json`:

```json
{ "name": "loyalty", "version": "1.0.0", "world": "discounts", "api": "0.1.0",
  "permissions": ["customers:read"],
  "settings": [{ "key": "points_per_euro", "label": "Points per euro" },
               { "key": "api_key", "label": "API key", "secret": true }] }
```

`api` is the `soat:plugin` version it was built against; before 1.0, a
plugin built for `0.1.x` runs on any `0.1` SOAT. Secret settings are
sealed and never shown again.

## Building one

Any language with a component toolchain works, for example Rust with
`cargo component`, against the WIT file above. Running plugins needs the
plugin host (roadmap F2) and the hook points wired into SOAT (F3).
