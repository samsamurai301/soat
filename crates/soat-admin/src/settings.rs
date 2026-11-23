//! Settings, by area: general (languages, domains), staff, developers (API
//! keys, webhooks) and Claude (approval of its changes). Each area is one
//! page; its forms post to `settings/{area}/{action}[/{item}]`.

use serde_json::{json, Map, Value};
use zapline::db::Db;
use zapline::prelude::*;

use crate::util::{encode, message};
use crate::{Ctx, Shared};

/// The areas, in the order the tabs show them.
const AREAS: &[&str] = &["general", "staff", "developers", "assistant"];

/// A form's fields; a name may repeat (checkboxes).
struct Fields(Vec<(String, String)>);

impl Fields {
    fn parse(body: &[u8]) -> Self {
        Self(serde_urlencoded::from_bytes(body).unwrap_or_default())
    }

    fn get(&self, name: &str) -> String {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.trim().to_owned())
            .unwrap_or_default()
    }

    fn all(&self, name: &str) -> Vec<String> {
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
        "developers" => {
            let keys = page_or!(cx.get("/api/api-keys").await);
            let hooks = page_or!(cx.get("/api/webhooks").await);
            let perms = page_or!(cx.get("/api/permissions").await);
            cx.set("keys", keys);
            cx.set("webhooks", hooks);
            cx.set("permissions", perms);
        }
        _ => {
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
                "staff" => "Staff",
                "developers" => "Developers",
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

async fn run(mut cx: Ctx, area: &str, action: &str, item: Option<&str>, f: Fields) -> Response {
    let item = item.map(encode);
    let call = match (area, action, item) {
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
        ("assistant", "approval", None) => Call::Plain(
            Method::PUT,
            "/api/assistant/settings".into(),
            Some(json!({ "approval": if f.get("review").is_empty() { "off" } else { "writes" } })),
        ),
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
}
