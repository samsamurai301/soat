# Plugins

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

## Where the hooks run

At the storefront checkout (`/api/storefront/carts/{id}/checkout` and the
HTML checkout):

1. `shipping-filter` once the address is known, on the options SOAT
   offers (the cart at its full prices).
2. `discounts` after SOAT's automatic discount and code, on what is left.
   A line's amount comes off that line; a cart's amount is spread over the
   lines in proportion, so tax stays right. The checkout lists them under
   `adjustments`.
3. `checkout-validation` when the order is placed: a block is a 422
   `order_blocked` with the hook's reason.

Hooks run in the order they were added, each seeing what the ones before
it did. Draft orders staff make are not hooked.

## Native hooks

An install can run hooks written in Rust today, without the plugin host:
implement `soat_core::hooks::Hook` (each method defaults to changing
nothing) and pass them in `Settings::hooks`:

```rust
let mut settings = soat::Settings::from_env()?;
settings.hooks = Hooks::new().with(MyLoyaltyDiscount);
```

## Installing plugins

Build with the `plugins` feature (on by default) and point
`SOAT_PLUGINS_DIR` at a folder holding one folder per plugin:

```
plugins/
  10-loyalty/
    soat.plugin.json
    plugin.wasm        # the component
    settings.json      # optional: {"amount": "250"}
```

They load at start, in folder name order, which is the order they run in.
`settings.json` holds the install's defaults; a shop's own settings go
over them.

## Turning one on for a shop

A plugin runs only for the shops that turn it on. A shop sees the plugins
the install offers in the admin under Settings, Plugins, or through the API:

- `GET /api/plugins` (`plugins:read`, staff have it): each plugin, whether
  it is on, and its settings. Secret ones only say whether they are `set`.
- `PUT /api/plugins/{name}` (`plugins:manage`, owners): `{"enabled": true,
  "settings": {"amount": "250", "api_key": "…"}}`. A setting the manifest
  does not declare is a 422 `unknown_setting`. Secret settings are sealed
  with `SOAT_SECRET_KEY` and never shown again; a secret left out or empty
  keeps the one stored. Publishes `plugin.configured`, without secrets.

Native hooks run for every shop.

## A shop's own plugins

A shop can also upload a plugin of its own, in the admin under Settings,
Plugins, or through the API. It is listed and turned on like the install's
(`"uploaded": true` and its `version`), runs for that shop alone in the
same sandbox, and runs after the install's hooks.

- `POST /api/plugins/uploads` (`plugins:manage`): `multipart/form-data`
  with the fields `manifest` (its `soat.plugin.json`) and `component` (the
  `.wasm`, at most 8 MiB). It must be a `discounts`, `shipping-filter` or
  `checkout-validation` plugin and load in the sandbox, or it is a 422
  `invalid_plugin`; a name one of the install's plugins uses is a 409
  `plugin_exists`. A new plugin starts turned off (201); uploading one the
  shop has again replaces it and keeps its settings (200). Publishes
  `plugin.uploaded` with its name, version, size and `sha256`, never the
  component.
- `DELETE /api/plugins/uploads/{name}` (`plugins:manage`): removes it and
  its settings. Publishes `plugin.removed`.

`events` plugins come only from the install, since they react for every
shop. An install built without the `plugins` feature answers uploads with
a 503 `plugins_unavailable`. Compiled plugins are kept in memory by the
fingerprints of their manifest and component, so a checkout compiles each
at most once.
A plugin that does not load (bad manifest, not a component, the wrong
world, or importing more than `host`) stops the server with the reason.

## The sandbox

The plugin host (`soat-plugin-host`, on wasmtime) gives every call a fresh
instance:

- **Fuel:** 50 million instructions a call (`PluginHost::with_fuel` to
  change it).
- **Memory:** 32 MiB an instance.
- **Imports:** `host` only. No WASI: no network, files, clock or
  randomness, so a call with the same cart gives the same answer.

A call that traps, runs out of fuel or exceeds memory changes nothing: a
`discounts` plugin takes nothing off, a `shipping-filter` keeps SOAT's
options, and a `checkout-validation` plugin lets the order through. The
reason goes to the plugin's log (`Plugin::log`, its last 200 lines, beside
what it wrote with `host.log`).

Loading in code:

```rust
let host = soat_plugin_host::PluginHost::new()?;
let plugin = host.load(manifest_json, &wasm, settings)?;
settings.hooks = Hooks::new().with(plugin);
```

## Building one

Any language with a component toolchain works, for example Rust with
`cargo component` or `wit-bindgen`, against the WIT file above.
`crates/soat-plugin-host/fixtures/loyalty` is a complete one in Rust
(no_std, 8 KB): it takes the `amount` setting off every line tagged
`plugin`; `fixtures/build.sh` builds it.

Next: the host moving into Zapline (`zapline-wasm`).
