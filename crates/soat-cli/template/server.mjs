// {{name}}: a SOAT app. Run `soat app dev --shop <shop>` once, then
// `node server.mjs`, and open the install link it printed.
import http from "node:http";
import fs from "node:fs";
import crypto from "node:crypto";

const env = Object.fromEntries(
  fs.readFileSync(".env", "utf8")
    .split("\n")
    .filter((l) => l.includes("="))
    .map((l) => [l.slice(0, l.indexOf("=")), l.slice(l.indexOf("=") + 1)]),
);
const SOAT = env.SOAT_URL;
const CLIENT_ID = env.SOAT_CLIENT_ID;
const PORT = Number(process.env.PORT || 3000);
const REDIRECT = `http://localhost:${PORT}/cb`;
const KEYS = "keys.json"; // shop -> key; keep it secret, keep it out of git

const keys = () => (fs.existsSync(KEYS) ? JSON.parse(fs.readFileSync(KEYS, "utf8")) : {});
const states = new Set();

async function soat(shop, path) {
  const res = await fetch(`${SOAT}${path}`, {
    headers: { authorization: `Bearer ${keys()[shop]}`, "x-soat-tenant": shop },
  });
  return res.json();
}

const send = (res, status, type, body) => {
  res.writeHead(status, { "content-type": type });
  res.end(body);
};
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://localhost:${PORT}`);
  const q = Object.fromEntries(url.searchParams);
  if (url.pathname === "/install") {
    // Where the directory sends a shop: on to SOAT's consent screen.
    const state = crypto.randomBytes(16).toString("hex");
    states.add(state);
    const to = new URL(`${SOAT}/admin/${encodeURIComponent(q.shop)}/apps/authorize`);
    to.search = new URLSearchParams({ client_id: CLIENT_ID, redirect_uri: REDIRECT, state });
    res.writeHead(302, { location: to.toString() });
    return res.end();
  }
  if (url.pathname === "/cb") {
    // The owner consented: trade the one-time code for the shop's key.
    if (!states.delete(q.state)) return send(res, 400, "text/plain", "Unknown state");
    const answer = await fetch(`${SOAT}/api/apps/token`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        client_id: CLIENT_ID, client_secret: env.SOAT_CLIENT_SECRET,
        code: q.code, shop: q.shop, redirect_uri: REDIRECT,
      }),
    }).then((r) => r.json());
    if (!answer.access_token) return send(res, 400, "text/plain", JSON.stringify(answer));
    fs.writeFileSync(KEYS, JSON.stringify({ ...keys(), [q.shop]: answer.access_token }, null, 2));
    return send(res, 200, "text/html", `<p>{{name}} is installed in ${esc(q.shop)}.</p>`);
  }
  if (url.pathname === "/admin") {
    // An admin link: the shop, and the product it was opened from.
    const products = await soat(q.shop, "/api/products?limit=10");
    const list = (products.items || []).map((p) => `<li>${esc(p.title)}</li>`).join("");
    return send(res, 200, "text/html",
      `<h1>{{name}}</h1><p>Opened from ${esc(q.id || "the Apps page")}.</p><ul>${list}</ul>`);
  }
  if (url.pathname === "/blocks/stars.js") {
    // A storefront block: fills the elements SOAT leaves for it.
    return send(res, 200, "text/javascript", `
      document.querySelectorAll('.soat-app-block[data-soat-app="${CLIENT_ID}"][data-block="stars"]')
        .forEach((el) => { el.textContent = "★★★★★"; });`);
  }
  send(res, 404, "text/plain", "Not found");
}).listen(PORT, () => console.log(`{{name}} on http://localhost:${PORT}`));
