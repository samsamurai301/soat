//! Settings, by area: general (languages, domains), payments, shipping,
//! taxes, email, invoices, staff, developers (API keys, webhooks) and
//! Claude (approval of its changes). Each area is one page; its forms post
//! to `settings/{area}/{action}[/{item}]`.

use serde_json::{json, Map, Value};
use zapline::db::Db;
use zapline::multipart::Multipart;
use zapline::prelude::*;

use soat_core::Currency;

use crate::util::{encode, message, money};
use crate::{Ctx, Shared};

/// The areas, in the order the tabs show them.
const AREAS: &[&str] = &[
    "general",
    "payments",
    "shipping",
    "taxes",
    "email",
    "invoices",
    "staff",
    "developers",
    "plugins",
    "assistant",
];

/// A percentage such as `19` or `7,5` in basis points.
fn bps(text: &str) -> Option<i64> {
    let eur = Currency::parse("EUR").ok()?;
    eur.parse_decimal(&text.trim().replace(',', "."))
        .filter(|b| (0..=10_000).contains(b))
}

/// An amount such as `4.95` in minor units of `currency`.
pub(crate) fn amount(text: &str, currency: &str) -> Option<i64> {
    Currency::parse(currency)
        .ok()?
        .parse_decimal(&text.trim().replace(',', "."))
        .filter(|a| *a >= 0)
}

fn percent(bps: &Value) -> String {
    let b = bps.as_i64().unwrap_or(0);
    if b % 100 == 0 {
        format!("{}%", b / 100)
    } else {
        let s = format!("{}.{:02}", b / 100, b % 100);
        format!("{}%", s.trim_end_matches('0'))
    }
}

/// A form's fields; a name may repeat (checkboxes).
pub(crate) struct Fields(Vec<(String, String)>);

impl Fields {
    pub(crate) fn parse(body: &[u8]) -> Self {
        Self(serde_urlencoded::from_bytes(body).unwrap_or_default())
    }

    pub(crate) fn get(&self, name: &str) -> String {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.trim().to_owned())
            .unwrap_or_default()
    }

    pub(crate) fn all(&self, name: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .collect()
    }

    /// A comma-separated field as a list.
    fn list(&self, name: &str) -> Vec<String> {
        self.get(name)
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// To the first area.
#[get("/admin/{shop}/settings", tag = "admin")]
pub(crate) async fn index(Path(shop): Path<String>) -> Response {
    Redirect::to(&format!("/admin/{}/settings/general", encode(&shop))).into_response()
}

/// One area of the settings.
#[get("/admin/{shop}/settings/{area}", tag = "admin")]
pub(crate) async fn show_area(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, area)): Path<(String, String)>,
    Query(q): Query<Map<String, Value>>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    if q.contains_key("done") {
        cx.set("done", json!("Saved."));
    }
    Ok(page(cx, &area, None, StatusCode::OK).await)
}

/// The area's page, with what an action left to say.
async fn page(mut cx: Ctx, area: &str, error: Option<String>, status: StatusCode) -> Response {
    if !AREAS.contains(&area) {
        return cx.failed(404, &Value::Null);
    }
    match area {
        "general" => {
            let langs = page_or!(cx.get("/api/languages").await);
            let others = langs["languages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            cx.set(
                "languages",
                json!({ "default": langs["default_language"], "others": others }),
            );
            let domains = page_or!(cx.get("/api/domains").await);
            cx.set("domains", domains);
        }
        "staff" => {
            let roles = page_or!(cx.get("/api/roles").await);
            let names: Vec<Value> = roles
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| r["name"].clone())
                .collect();
            let staff = page_or!(cx.get("/api/staff").await);
            let staff: Vec<Value> = staff
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| {
                    let options: Vec<Value> = names
                        .iter()
                        .map(|n| json!({ "name": n, "selected": *n == m["role"] }))
                        .collect();
                    json!({ "user_id": m["user_id"], "email": m["email"], "role": m["role"], "roles": options })
                })
                .collect();
            cx.set("staff", Value::Array(staff));
            cx.set("roles", roles);
        }
        "payments" => {
            let methods = page_or!(cx.get("/api/payment-methods").await);
            let providers = page_or!(cx.get("/api/payment-providers").await);
            cx.set("methods", methods["items"].clone());
            cx.set("providers", providers["items"].clone());
        }
        "shipping" => {
            let zones = page_or!(cx.get("/api/shipping/zones").await);
            let zones: Vec<Value> = zones["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|z| {
                    let rates: Vec<Value> = z["rates"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|r| {
                            let cur = r["currency"].as_str().unwrap_or_default();
                            let days = match (r["min_days"].as_i64(), r["max_days"].as_i64()) {
                                (Some(a), Some(b)) if a == b => format!("{a} days"),
                                (Some(a), Some(b)) => format!("{a}–{b} days"),
                                (None, Some(b)) => format!("up to {b} days"),
                                (Some(a), None) => format!("from {a} days"),
                                _ => String::new(),
                            };
                            json!({
                                "id": r["id"], "name": r["name"], "carrier": r["carrier"],
                                "price": money(r["price_minor"].as_i64().unwrap_or(0), cur),
                                "free_from": r["min_subtotal_minor"].as_i64().map(|m| money(m, cur)),
                                "days": days,
                            })
                        })
                        .collect();
                    json!({ "id": z["id"], "name": z["name"], "countries": z["countries"], "rates": rates })
                })
                .collect();
            cx.set("zones", Value::Array(zones));
        }
        "taxes" => {
            let settings = page_or!(cx.get("/api/tax/settings").await);
            let zones = page_or!(cx.get("/api/tax/zones?limit=200").await);
            let zones: Vec<Value> = zones["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|z| {
                    let rates: Vec<String> = z["rates"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|r| format!("{} {}", r["tax_class"].as_str().unwrap_or_default(), percent(&r["rate_bps"])))
                        .collect();
                    json!({ "id": z["id"], "name": z["name"], "countries": z["countries"], "rates": rates.join(", ") })
                })
                .collect();
            cx.set("tax", settings);
            cx.set("zones", Value::Array(zones));
        }
        "email" => {
            let settings = page_or!(cx.get("/api/notifications/settings").await);
            let templates = page_or!(cx.get("/api/notifications/templates").await);
            let recovery = page_or!(cx.get("/api/cart-recovery").await);
            cx.set("email", settings);
            cx.set("templates", templates);
            cx.set("recovery", recovery);
        }
        "invoices" => {
            let (status, settings) = cx.call(Method::GET, "/api/invoices/settings", None).await;
            match status {
                200 => cx.set("invoice", settings),
                404 => cx.set("invoice", json!({ "issue_on": "paid", "address": {} })),
                401 => return Redirect::to("/admin/login").into_response(),
                _ => return cx.failed(status, &settings),
            }
            let on = cx.data["invoice"]["issue_on"]
                .as_str()
                .unwrap_or("paid")
                .to_owned();
            cx.set("issue_on", json!({ on: true }));
        }
        "developers" => {
            let keys = page_or!(cx.get("/api/api-keys").await);
            let hooks = page_or!(cx.get("/api/webhooks").await);
            let perms = page_or!(cx.get("/api/permissions").await);
            cx.set("keys", keys);
            cx.set("webhooks", hooks);
            cx.set("permissions", perms);
        }
        "plugins" => {
            let plugins = page_or!(cx.get("/api/plugins").await);
            cx.set("plugins", plugins);
        }
        _ => {
            // Staff who may not manage the assistant do not see its key.
            let (status, key) = cx.call(Method::GET, "/api/assistant/key", None).await;
            if status == 200 {
                cx.set("key", key);
            }
            let settings = page_or!(cx.get("/api/assistant/settings").await);
            let pending = page_or!(
                cx.get("/api/assistant/proposals?status=pending&limit=50")
                    .await
            );
            let pending: Vec<Value> = pending["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| {
                    json!({
                        "id": p["id"], "method": p["method"], "path": p["path"],
                        "reason": p["reason"], "tool": p["tool"],
                        "body": p["body"].as_object().map(|_| {
                            serde_json::to_string_pretty(&p["body"]).unwrap_or_default()
                        }),
                        "created": p["created_at"].as_str().and_then(|d| d.get(..16)),
                    })
                })
                .collect();
            cx.set("review", json!(settings["approval"] == "writes"));
            cx.set("proposals", Value::Array(pending));
        }
    }
    let tabs: Vec<Value> = AREAS
        .iter()
        .map(|a| {
            let label = match *a {
                "general" => "General",
                "payments" => "Payments",
                "shipping" => "Shipping",
                "taxes" => "Taxes",
                "email" => "Email",
                "invoices" => "Invoices",
                "staff" => "Staff",
                "developers" => "Developers",
                "plugins" => "Plugins",
                _ => "Claude",
            };
            json!({ "area": a, "label": label, "current": *a == area })
        })
        .collect();
    let title = tabs
        .iter()
        .find(|t| t["current"] == true)
        .and_then(|t| t["label"].as_str())
        .unwrap_or("Settings")
        .to_owned();
    cx.set("tabs", Value::Array(tabs));
    cx.set("page_title", json!(format!("Settings: {title}")));
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render(&format!("settings_{area}"), status)
}

/// A settings form without an item (a form post).
#[post("/admin/{shop}/settings/{area}/{action}", tag = "admin")]
pub(crate) async fn act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, area, action)): Path<(String, String, String)>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    Ok(run(cx, &area, &action, None, Fields::parse(&body)).await)
}

/// A settings form about one item, such as a domain or a key (a form
/// post).
#[post("/admin/{shop}/settings/{area}/{action}/{item}", tag = "admin")]
pub(crate) async fn act_on(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, area, action, item)): Path<(String, String, String, String)>,
    body: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    Ok(run(cx, &area, &action, Some(&item), Fields::parse(&body)).await)
}

/// What an action asks of the API.
enum Call {
    /// Back to the area on success.
    Plain(Method, String, Option<Value>),
    /// The answer holds a secret shown once: the area again, with it.
    Secret(Method, String, Value, &'static str),
}

/// The area again with `error`, for a form that never reached the API.
async fn refused(cx: Ctx, area: &str, error: &str) -> Response {
    page(
        cx,
        area,
        Some(error.to_owned()),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await
}

async fn run(mut cx: Ctx, area: &str, action: &str, item: Option<&str>, f: Fields) -> Response {
    let item = item.map(encode);
    let opt = |name: &str| {
        let v = f.get(name);
        (!v.is_empty()).then_some(v)
    };
    let call = match (area, action, item) {
        ("payments", "add", None) => {
            let provider = f.get("provider");
            let settings = if provider == "manual" {
                json!({ "instructions": f.get("instructions") })
            } else {
                match opt("settings").map(|s| serde_json::from_str::<Value>(&s)) {
                    None => json!({}),
                    Some(Ok(v)) => v,
                    Some(Err(_)) => {
                        return refused(cx, area, "The settings are not valid JSON.").await
                    }
                }
            };
            Call::Plain(
                Method::POST,
                "/api/payment-methods".into(),
                Some(json!({ "provider": provider, "name": f.get("name"), "settings": settings })),
            )
        }
        ("payments", toggle @ ("enable" | "disable"), Some(m)) => Call::Plain(
            Method::PATCH,
            format!("/api/payment-methods/{m}"),
            Some(json!({ "active": toggle == "enable" })),
        ),
        ("payments", "remove", Some(m)) => {
            Call::Plain(Method::DELETE, format!("/api/payment-methods/{m}"), None)
        }
        ("shipping", "add-zone", None) => Call::Plain(
            Method::POST,
            "/api/shipping/zones".into(),
            Some(json!({ "name": f.get("name"), "countries": upper(f.list("countries")) })),
        ),
        ("shipping", "remove-zone", Some(z)) => {
            Call::Plain(Method::DELETE, format!("/api/shipping/zones/{z}"), None)
        }
        ("shipping", "add-rate", Some(z)) => {
            let currency = f.get("currency").to_ascii_uppercase();
            let Some(price) = amount(&f.get("price"), &currency) else {
                return refused(
                    cx,
                    area,
                    &format!("The price is an amount in {currency}, like 4.95."),
                )
                .await;
            };
            let free_from = match opt("free_from") {
                None => None,
                Some(t) => match amount(&t, &currency) {
                    Some(a) => Some(a),
                    None => {
                        return refused(cx, area, "“Free from” is an amount, like 50.00.").await
                    }
                },
            };
            let days = |n: &str| opt(n).and_then(|d| d.parse::<i32>().ok());
            let base = json!({
                "name": f.get("name"), "carrier": opt("carrier"), "currency": currency,
                "min_days": days("min_days"), "max_days": days("max_days"),
            });
            // A free-shipping threshold is a second rate at 0 from that subtotal,
            // and the paid one stops below it.
            let mut paid = base.clone();
            paid["price_minor"] = json!(price);
            if let Some(m) = free_from {
                paid["max_subtotal_minor"] = json!(m - 1);
                let mut free = base;
                free["price_minor"] = json!(0);
                free["min_subtotal_minor"] = json!(m);
                let path = format!("/api/shipping/zones/{z}/rates");
                let (status, out) = cx.call(Method::POST, &path, Some(&free)).await;
                if !(200..300).contains(&status) {
                    let code =
                        StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
                    return page(cx, area, Some(message(&out)), code).await;
                }
            }
            Call::Plain(
                Method::POST,
                format!("/api/shipping/zones/{z}/rates"),
                Some(paid),
            )
        }
        ("shipping", "remove-rate", Some(r)) => {
            Call::Plain(Method::DELETE, format!("/api/shipping/rates/{r}"), None)
        }
        ("taxes", "settings", None) => Call::Plain(
            Method::PUT,
            "/api/tax/settings".into(),
            Some(json!({
                "origin_country": opt("origin_country").map(|c| c.to_ascii_uppercase()),
                "prices_include_tax": !f.get("prices_include_tax").is_empty(),
                "charge_destination_vat": !f.get("charge_destination_vat").is_empty(),
            })),
        ),
        ("taxes", "eu-vat", None) => Call::Plain(
            Method::POST,
            "/api/tax/presets/eu-vat".into(),
            Some(json!({})),
        ),
        ("taxes", "add-zone", None) => {
            let mut rates = Vec::new();
            for (class, field) in [("standard", "standard"), ("reduced", "reduced")] {
                if let Some(t) = opt(field) {
                    let Some(b) = bps(&t) else {
                        return refused(cx, area, "A rate is a percentage, like 19 or 7.5.").await;
                    };
                    rates.push(json!({ "tax_class": class, "rate_bps": b }));
                }
            }
            Call::Plain(
                Method::POST,
                "/api/tax/zones".into(),
                Some(
                    json!({ "name": f.get("name"), "countries": upper(f.list("countries")), "rates": rates }),
                ),
            )
        }
        ("taxes", "remove-zone", Some(z)) => {
            Call::Plain(Method::DELETE, format!("/api/tax/zones/{z}"), None)
        }
        ("email", "cart-recovery", None) => Call::Plain(
            Method::PUT,
            "/api/cart-recovery".into(),
            Some(json!({
                "enabled": f.get("enabled") == "on",
                "delay_minutes": f.get("delay_minutes").trim().parse::<i64>().ok(),
            })),
        ),
        ("email", "settings", None) => {
            // Unticked templates are turned off.
            let templates = page_or!(cx.get("/api/notifications/templates").await);
            let on = f.all("enabled");
            let disabled: Vec<Value> = templates
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["key"].as_str())
                .filter(|k| !on.iter().any(|o| o == k))
                .map(|k| json!(k))
                .collect();
            Call::Plain(
                Method::PUT,
                "/api/notifications/settings".into(),
                Some(json!({
                    "sender_name": f.get("sender_name"), "reply_to": opt("reply_to"),
                    "staff_email": opt("staff_email"), "disabled": disabled,
                })),
            )
        }
        ("invoices", "settings", None) => Call::Plain(
            Method::PUT,
            "/api/invoices/settings".into(),
            Some(json!({
                "legal_name": f.get("legal_name"),
                "address": {
                    "line1": f.get("line1"), "line2": f.get("line2"),
                    "postal_code": f.get("postal_code"), "city": f.get("city"),
                    "country": f.get("country").to_ascii_uppercase(),
                },
                "vat_number": opt("vat_number"), "tax_number": opt("tax_number"),
                "email": opt("email"), "bank_details": f.get("bank_details"),
                "footer": f.get("footer"), "invoice_prefix": opt("invoice_prefix"),
                "credit_note_prefix": opt("credit_note_prefix"), "issue_on": opt("issue_on"),
            })),
        ),
        ("general", "languages", None) => Call::Plain(
            Method::PUT,
            "/api/languages".into(),
            Some(
                json!({ "default_language": f.get("default_language"), "languages": f.list("languages") }),
            ),
        ),
        ("general", "domains", None) => Call::Plain(
            Method::POST,
            "/api/domains".into(),
            Some(json!({ "domain": f.get("domain") })),
        ),
        ("general", "remove-domain", Some(d)) => {
            Call::Plain(Method::DELETE, format!("/api/domains/{d}"), None)
        }
        ("staff", "add", None) => {
            let password = f.get("password");
            Call::Plain(
                Method::POST,
                "/api/staff".into(),
                Some(json!({
                    "email": f.get("email"),
                    "password": (!password.is_empty()).then_some(password),
                    "role": f.get("role"),
                })),
            )
        }
        ("staff", "role", Some(u)) => Call::Plain(
            Method::PATCH,
            format!("/api/staff/{u}"),
            Some(json!({ "role": f.get("role") })),
        ),
        ("developers", "keys", None) => Call::Secret(
            Method::POST,
            "/api/api-keys".into(),
            json!({ "name": f.get("name"), "permissions": f.all("permissions") }),
            "API key",
        ),
        ("developers", "revoke-key", Some(k)) => {
            Call::Plain(Method::DELETE, format!("/api/api-keys/{k}"), None)
        }
        ("developers", "webhooks", None) => {
            let mut events = f.list("events");
            if events.is_empty() {
                events.push("*".into());
            }
            Call::Secret(
                Method::POST,
                "/api/webhooks".into(),
                json!({ "url": f.get("url"), "events": events }),
                "webhook signing secret",
            )
        }
        ("developers", "remove-webhook", Some(w)) => {
            Call::Plain(Method::DELETE, format!("/api/webhooks/{w}"), None)
        }
        ("plugins", "save", Some(name)) => {
            let plugins = match cx.get("/api/plugins").await {
                Ok(p) => p,
                Err(r) => return *r,
            };
            let Some(plugin) = plugins
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| p["name"].as_str().map(encode).as_deref() == Some(name.as_str()))
            else {
                return cx.failed(404, &Value::Null);
            };
            let settings: serde_json::Map<String, Value> = plugin["settings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["key"].as_str())
                .map(|k| (k.to_owned(), json!(f.get(&format!("setting_{k}")))))
                .collect();
            Call::Plain(
                Method::PUT,
                format!("/api/plugins/{name}"),
                Some(json!({ "enabled": !f.get("enabled").is_empty(), "settings": settings })),
            )
        }
        ("plugins", "remove", Some(name)) => {
            Call::Plain(Method::DELETE, format!("/api/plugins/uploads/{name}"), None)
        }
        ("assistant", "approval", None) => Call::Plain(
            Method::PUT,
            "/api/assistant/settings".into(),
            Some(json!({ "approval": if f.get("review").is_empty() { "off" } else { "writes" } })),
        ),
        ("assistant", "key", None) => Call::Plain(
            Method::PUT,
            "/api/assistant/key".into(),
            Some(json!({ "api_key": f.get("api_key"), "model": f.get("model") })),
        ),
        ("assistant", "remove-key", None) => {
            Call::Plain(Method::DELETE, "/api/assistant/key".into(), None)
        }
        ("assistant", verdict @ ("approve" | "reject"), Some(p)) => Call::Plain(
            Method::POST,
            format!("/api/assistant/proposals/{p}/{verdict}"),
            Some(json!({})),
        ),
        _ => return cx.failed(404, &Value::Null),
    };
    let (status, out, secret) = match call {
        Call::Plain(method, path, body) => {
            let (s, o) = cx.call(method, &path, body.as_ref()).await;
            (s, o, None)
        }
        Call::Secret(method, path, body, what) => {
            let (s, o) = cx.call(method, &path, Some(&body)).await;
            (s, o, Some(what))
        }
    };
    match (status, secret) {
        (200..=299, None) => {
            Redirect::to(&format!("/admin/{}/settings/{area}?done=1", cx.shop)).into_response()
        }
        (200..=299, Some(what)) => {
            // Shown once, never stored by the admin.
            let value = out["secret"].clone();
            cx.set("secret", json!({ "what": what, "value": value }));
            page(cx, area, None, StatusCode::CREATED).await
        }
        (401, _) => Redirect::to("/admin/login").into_response(),
        (403 | 404, _) => cx.failed(status, &out),
        _ => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            page(cx, area, Some(message(&out)), code).await
        }
    }
}

/// Upload a plugin for this shop alone (a `multipart/form-data` form post
/// with the files `manifest`, its `soat.plugin.json`, and `component`, the
/// `.wasm`).
#[post("/admin/{shop}/settings/plugins/upload", tag = "admin")]
pub(crate) async fn upload_plugin(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    mut form: Multipart,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    let mut manifest = None;
    let mut component = None;
    while let Ok(Some(field)) = form.next_field().await {
        match field.name() {
            Some("manifest") => manifest = field.bytes().await.ok(),
            Some("component") => component = field.bytes().await.ok(),
            _ => {}
        }
    }
    let manifest = manifest
        .filter(|m| !m.is_empty())
        .and_then(|m| String::from_utf8(m.to_vec()).ok());
    let (Some(manifest), Some(component)) = (manifest, component.filter(|c| !c.is_empty())) else {
        let e = "Choose the plugin's soat.plugin.json and its .wasm component.";
        return Ok(page(
            cx,
            "plugins",
            Some(e.into()),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await);
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let boundary = format!("----soat-plugin-{nanos:x}-{:x}", component.len());
    let mut body = Vec::with_capacity(component.len() + manifest.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\ncontent-disposition: form-data; name=\"manifest\"\r\n\r\n{manifest}\r\n\
             --{boundary}\r\ncontent-disposition: form-data; name=\"component\"; filename=\"plugin.wasm\"\r\n\
             content-type: application/wasm\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&component);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let (status, out) =
        cx.s.api
            .send_raw(
                &cx.db,
                Method::POST,
                "/api/plugins/uploads",
                Bytes::from(body),
                &format!("multipart/form-data; boundary={boundary}"),
                &cx.token,
                &cx.shop,
                cx.conn,
            )
            .await;
    Ok(match status {
        200 | 201 => {
            Redirect::to(&format!("/admin/{}/settings/plugins?done=1", cx.shop)).into_response()
        }
        401 => Redirect::to("/admin/login").into_response(),
        403 => cx.failed(403, &out),
        _ => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            page(cx, "plugins", Some(message(&out)), code).await
        }
    })
}

/// An email's wording to edit.
#[get("/admin/{shop}/emails/{key}", tag = "admin")]
pub(crate) async fn email_template(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, key)): Path<(String, String)>,
    Query(q): Query<Map<String, Value>>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    let t = page!(
        cx.get(&format!("/api/notifications/templates/{}", encode(&key)))
            .await
    );
    cx.set("saved", json!(q.contains_key("saved")));
    cx.set("page_title", json!(format!("Email: {key}")));
    cx.set("template", t);
    Ok(cx.render("email_template", StatusCode::OK))
}

/// The email form's fields.
#[model]
pub struct TemplateForm {
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
}

/// Reword an email (a form post); a template that does not parse is shown
/// again with the reason.
#[post("/admin/{shop}/emails/{key}", tag = "admin")]
pub(crate) async fn save_email_template(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, key)): Path<(String, String)>,
    Form(f): Form<TemplateForm>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    let crlf = |t: &str| t.replace("\r\n", "\n");
    let body = json!({
        "subject": f.subject.trim(), "text": crlf(&f.text),
        "html": crlf(f.html.as_deref().unwrap_or_default()),
    });
    let path = format!("/api/notifications/templates/{}", encode(&key));
    let (status, out) = cx.call(Method::PUT, &path, Some(&body)).await;
    Ok(match status {
        200 => {
            Redirect::to(&format!("/admin/{shop}/emails/{}?saved=1", encode(&key))).into_response()
        }
        401 => Redirect::to("/admin/login").into_response(),
        422 => {
            let current = page!(cx.get(&path).await);
            let mut t = body;
            t["key"] = json!(key);
            t["description"] = current["description"].clone();
            t["customized"] = current["customized"].clone();
            cx.set("page_title", json!(format!("Email: {key}")));
            cx.set("template", t);
            cx.set("error", json!(message(&out)));
            cx.render("email_template", StatusCode::UNPROCESSABLE_ENTITY)
        }
        _ => cx.failed(status, &out),
    })
}

/// Back to an email's default wording (a form post).
#[post("/admin/{shop}/emails/{key}/reset", tag = "admin")]
pub(crate) async fn reset_email_template(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, key)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "settings").await);
    let path = format!("/api/notifications/templates/{}", encode(&key));
    let (status, out) = cx.call(Method::DELETE, &path, None).await;
    Ok(match status {
        200 | 204 => {
            Redirect::to(&format!("/admin/{shop}/emails/{}", encode(&key))).into_response()
        }
        401 => Redirect::to("/admin/login").into_response(),
        _ => cx.failed(status, &out),
    })
}

fn upper(v: Vec<String>) -> Vec<String> {
    v.into_iter().map(|c| c.to_ascii_uppercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_fields_repeat() {
        let f = Fields::parse(b"name=ERP&permissions=products%3Aread&permissions=orders%3Aread&permissions=&events=+a.b%2C+c.d+");
        assert_eq!(f.get("name"), "ERP");
        assert_eq!(f.all("permissions"), ["products:read", "orders:read"]);
        assert_eq!(f.list("events"), ["a.b", "c.d"]);
        assert_eq!(f.get("missing"), "");
    }

    #[test]
    fn rates_and_amounts() {
        assert_eq!(bps("19"), Some(1900));
        assert_eq!(bps("7,5"), Some(750));
        assert_eq!(bps("101"), None);
        assert_eq!(bps("x"), None);
        assert_eq!(amount("4,95", "EUR"), Some(495));
        assert_eq!(amount("500", "JPY"), Some(500));
        assert_eq!(amount("-1", "EUR"), None);
        assert_eq!(percent(&json!(1900)), "19%");
        assert_eq!(percent(&json!(750)), "7.5%");
        assert_eq!(percent(&json!(1025)), "10.25%");
    }
}
