# Security

What SOAT does on every request, before any module sees it.

## Headers

Every response carries browser hardening headers:

- `X-Content-Type-Options: nosniff`
- `X-Frame-Options: DENY`
- `Referrer-Policy: strict-origin-when-cross-origin`
- `Cross-Origin-Opener-Policy: same-origin`
- a `Permissions-Policy` that turns off camera, microphone, geolocation and
  the like

## Content Security Policy

Every HTML page carries a `Content-Security-Policy` with a fresh nonce
(`crates/soat/src/csp.rs`). A script runs only when it carries the nonce,
and only the page's own templates can give it one: when a template is
loaded, each `<script>` and `<style>` element in its source gets the
nonce's placeholder (`soat_core::csp::tag`). The server puts the real nonce
in place of the placeholder in each response. A `<script>` that reaches a
page any other way, through content or a customer's input, has no nonce
and the browser refuses it. Inline event handlers (`onclick="..."`) and
`javascript:` links never run.

- **Storefront:** a trusted script may load others (`'strict-dynamic'`). So
  the theme's scripts, tracking after consent (Google Analytics, Meta Pixel)
  and apps' storefront scripts work without listing their hosts. Images,
  styles, fonts and requests may come from any HTTPS origin.
- **Admin:** stricter. Its own scripts and styles only, images from
  anywhere over HTTPS, forms posted only to SOAT itself.
- **Neither** can be framed (`frame-ancestors 'none'`), and neither loads
  plugins (`object-src 'none'`) or takes another `<base>`.
- **Exempt:** the API docs (`/docs`, whose viewer comes from a CDN) and the
  development mail inbox.

A shop's own theme files need nothing special. Their `<script>` and
`<style>` elements are tagged the same way, saved files included. Write
behaviour as script elements with listeners, not as `on...` attributes.

## Forms from other sites

The admin and the storefront use cookies and take form posts. A browser may
post to them only from the same origin. SOAT reads `Sec-Fetch-Site`, or
`Origin` on older browsers, and answers `403 cross_origin` to a post that
another site, or a sibling subdomain, sent. This stops cross-site request
forgery without tokens in forms.

`/api/` is exempt. It takes tokens in headers, never cookies, and answers
other origins by design, for headless storefronts and apps. Requests from
outside a browser, such as `curl`, scripts or payment providers' webhooks,
carry neither header and always pass.

## Guessing

Each client gets 20 attempts a minute, per endpoint, wherever a password,
code or link is checked:

- staff sign-in and first-time setup;
- customer sign-in, sign-up, sign-in links and password help;
- the gift card balance;
- cart recovery links;
- app token exchange;
- newsletter unsubscribe links.

The allowance refills evenly over the minute. Beyond it the answer is
`429 rate_limited` with `Retry-After`. Nothing else is limited.

Behind a reverse proxy, every request seems to come from the proxy. Set
`TRUSTED_PROXIES` to the proxies' addresses or networks (comma-separated,
e.g. `10.0.0.0/8,127.0.0.1`), so SOAT reads the client from
`X-Forwarded-For`. A malformed value stops the server at start.

Staff and customer passwords are stored as Argon2id hashes, and API keys,
sessions and links as SHA-256 hashes, never in plain text.

## Links shops give the server

Webhooks and image imports make SOAT call addresses a shop chose, so
they could otherwise point it at its own network (cloud metadata, the
database). Only `https://` to public addresses is allowed. Both check
the address the connection is made to (`soat_webhooks::PublicResolver`),
so a name that resolves to a private address is refused even if it
changed since it was checked. Image imports (`soat_http::fetch`) check
each of up to three redirects again and keep at most 20 MB of a type SOAT
stores; webhooks follow no redirects. `SOAT_ALLOW_PRIVATE_URLS=1` lifts all of this
for local development; never set it on a server.

## Domains

A shop's domain decides which shop a request without a key or header
acts for, so on an install for many shops nobody may hold a domain they
do not own. Adding one (`POST /api/domains`, or admin Settings, General)
gives a TXT record to add at `_soat-challenge.<domain>`; `POST
/api/domains/{domain}/verify` looks it up and only then does the domain
serve the shop. Any number of shops may claim a domain and the first to
prove it gets it, so a claim cannot block the owner. An install for one
shop, or whose operator adds the domains, sets `SOAT_VERIFY_DOMAINS=0`
and domains serve the shop as soon as they are added.
