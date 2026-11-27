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
