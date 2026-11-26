# {{name}}

A SOAT app. It needs Node 18 or newer and no packages.

1. `soat login https://your-soat.example.com you@example.com`
2. `soat app dev --shop <shop>` registers the app from `soat.app.json`
   (the first time; later it updates it), writes `.env`, and prints the link
   that installs it in the shop.
3. `node server.mjs`, then open that link.

Change `soat.app.json` (permissions, admin links, storefront blocks) and run
`soat app dev` again. Shops keep the permissions they granted until they
install the app again.

The guide to apps: `docs/apps.md` in SOAT.
