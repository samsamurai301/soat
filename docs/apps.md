# Building an app for SOAT

An app is software you host that works for SOAT shops. It acts through the
shop API with a key that holds exactly the permissions the shop's owner
granted, and it can add links to the admin and blocks to the storefront.

## 1. Register

Sign in as any SOAT user and register the app. The secret is shown once.

```http
POST /api/apps
{ "name": "Reviews",
  "redirect_uris": ["https://reviews.example.com/cb"],
  "permissions": ["products:read", "orders:read"] }
-> 201 { "app": { "id": "app_…", … }, "client_secret": "soat_cs_…" }
```

Redirect addresses are `https://…`, or `http://localhost…` while
developing. `POST /api/apps/{id}/secret` replaces a lost secret.

## 2. Install (authorization code flow)

Send the shop's owner to the consent screen:

```
/admin/{shop}/apps/authorize?client_id=app_…&redirect_uri=https://reviews.example.com/cb&state=…
```

When they approve, SOAT sends them to your address with `code`, `shop`
and your `state`. Check `state`, then trade the code within 10 minutes:

```http
POST /api/apps/token
{ "client_id": "app_…", "client_secret": "soat_cs_…", "code": "…",
  "shop": "acme", "redirect_uri": "https://reviews.example.com/cb" }
-> 200 { "access_token": "soat_sk_acme_…", "shop": "acme", "permissions": [ … ] }
```

Send the key as `Authorization: Bearer soat_sk_…`. A code works once.
When the shop uninstalls the app, the key stops working at once and the
shop's event log shows `app.uninstalled`.

## 3. Describe the app

`PATCH /api/apps/{id}` (its owner) changes any of these:

| Field | What it does |
| --- | --- |
| `tagline`, `description`, `category` | What the directory shows. |
| `app_url` | Where installing starts: SOAT links to it with `?shop=…`, and your page sends the owner on to the consent screen. |
| `listed` | Shows the app in the directory (needs `tagline` and `app_url`). |
| `admin_links` | Up to 10 `{ target, label, url }`; `target` is `app` (the Apps page), `product`, `order` or `customer`. The link opens with `shop` and, on a record's page, `id`. |
| `storefront_blocks` | Up to 10 `{ handle, name, placement, script_url }`; `placement` is `body` or `product`. |

## 4. Storefront blocks

The owner turns each block on for their shop on the Apps page. The default
theme then loads `script_url` once (deferred) and leaves an element for
each block that is on:

```html
<div class="soat-app-blocks" data-product="prod_…">            <!-- product pages -->
  <div class="soat-app-block" data-soat-app="app_…" data-block="stars"></div>
</div>
<div class="soat-app-block" data-soat-app="app_…" data-block="widget"></div>  <!-- every page -->
```

Your script finds its elements with
`document.querySelectorAll('.soat-app-block[data-soat-app="app_…"]')` and
fills them. Read public data from the storefront API (`/api/storefront/…`,
see `headless.md`), not with the shop's key: anything in the browser is
public.
