//! Collections: a list, and a page per collection to name it and either
//! pick its products by hand (add by search, order, remove) or let rules
//! pick them (smart collections).

use serde_json::{json, Value};
use soat_core::Currency;
use zapline::db::Db;
use zapline::prelude::*;

use crate::settings::{amount, Fields};
use crate::util::{encode, message};
use crate::{Ctx, Shared};

/// Rule rows the form shows.
const ROWS: usize = 5;

const FIELDS: &[(&str, &str)] = &[
    ("tag", "Tag"),
    ("title", "Title"),
    ("sku", "SKU"),
    ("price_minor", "Price"),
    ("weight_grams", "Weight (g)"),
    ("tax_class", "Tax class"),
];

const OPS: &[(&str, &str)] = &[
    ("equals", "is"),
    ("not_equals", "is not"),
    ("contains", "contains"),
    ("not_contains", "does not contain"),
    ("starts_with", "starts with"),
    ("greater_than", "is more than"),
    ("less_than", "is less than"),
];

/// The shop's currency for prices in rules.
async fn currency(cx: &Ctx) -> String {
    let (_, shop) = cx.call(Method::GET, "/api/shop", None).await;
    shop["currency"].as_str().unwrap_or("EUR").to_owned()
}

/// The rule rows for the form: each rule, then empty rows.
fn rows(rules: &Value, currency: &str) -> Vec<Value> {
    let c = Currency::parse(currency).ok();
    let mut out: Vec<Value> = rules
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let value = match (r["field"].as_str(), r["value"].as_str(), c) {
                (Some("price_minor"), Some(v), Some(c)) => {
                    c.decimal(v.parse::<i64>().unwrap_or(0))
                }
                (_, v, _) => v.unwrap_or_default().to_owned(),
            };
            (r["field"].clone(), r["op"].clone(), value)
        })
        .chain(std::iter::repeat((Value::Null, Value::Null, String::new())))
        .take(ROWS.max(rules.as_array().map_or(0, Vec::len)))
        .enumerate()
        .map(|(i, (field, op, value))| {
            json!({
                "i": i,
                "fields": FIELDS.iter().map(|(k, l)| json!({ "key": k, "label": l, "selected": field == *k })).collect::<Vec<_>>(),
                "ops": OPS.iter().map(|(k, l)| json!({ "key": k, "label": l, "selected": op == *k })).collect::<Vec<_>>(),
                "value": value,
            })
        })
        .collect();
    out.truncate(ROWS.max(out.len()));
    out
}

/// Rules from the form's rows; prices typed as amounts in `currency`.
fn parse_rules(f: &Fields, currency: &str) -> std::result::Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for i in 0..ROWS {
        let value = f.get(&format!("value{i}"));
        if value.is_empty() {
            continue;
        }
        let field = f.get(&format!("field{i}"));
        let value = if field == "price_minor" {
            amount(&value, currency)
                .ok_or_else(|| format!("{value} is not a price in {currency}."))?
                .to_string()
        } else {
            value
        };
        out.push(json!({ "field": field, "op": f.get(&format!("op{i}")), "value": value }));
    }
    Ok(out)
}

/// The shop's collections.
#[get("/admin/{shop}/collections", tag = "admin")]
pub(crate) async fn list(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let mut cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let page = page!(cx.get("/api/collections?limit=200&sort=title").await);
    let items: Vec<Value> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let mut c = c.clone();
            c["smart"] = json!(c["rules"].is_array());
            c
        })
        .collect();
    cx.set("collections", Value::Array(items));
    cx.set("page_title", json!("Collections"));
    Ok(cx.render("collections", StatusCode::OK))
}

async fn show(
    mut cx: Ctx,
    collection: Value,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let currency = currency(&cx).await;
    let smart = collection["rules"].is_array();
    cx.set("rows", json!(rows(&collection["rules"], &currency)));
    if let Some(id) = collection["id"].as_str() {
        let (_, products) = cx
            .call(
                Method::GET,
                &format!("/api/collections/{}/products?limit=200", encode(id)),
                None,
            )
            .await;
        let items: Vec<Value> = products["items"].as_array().cloned().unwrap_or_default();
        cx.set("count", json!(products["total"]));
        cx.set("products", Value::Array(items));
    }
    cx.set(
        "page_title",
        json!(collection["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .unwrap_or("New collection")),
    );
    cx.set("creating", json!(collection["id"].is_null()));
    cx.set("smart", json!(smart));
    cx.set("any", json!(collection["match_all"] == json!(false)));
    cx.set("currency", json!(currency));
    cx.set("collection", collection);
    if let Some(e) = error {
        cx.set("error", json!(e));
    }
    cx.render("collection", status)
}

/// The form for a new collection.
#[get("/admin/{shop}/collections/new", tag = "admin")]
pub(crate) async fn new_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    Ok(show(
        cx,
        json!({ "title": "", "rules": null }),
        None,
        StatusCode::OK,
    )
    .await)
}

/// A collection's page.
#[get("/admin/{shop}/collections/{id}", tag = "admin")]
pub(crate) async fn edit_page(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let c = page!(cx.get(&format!("/api/collections/{}", encode(&id))).await);
    Ok(show(cx, c, None, StatusCode::OK).await)
}

/// The form's collection as the API takes it.
fn body(f: &Fields, currency: &str) -> std::result::Result<Value, String> {
    let smart = f.get("kind") == "smart";
    let rules = if smart {
        parse_rules(f, currency)?
    } else {
        Vec::new()
    };
    if smart && rules.is_empty() {
        return Err("Give a smart collection at least one rule.".into());
    }
    let mut b = json!({
        "title": f.get("title"), "description": f.get("description"),
        "seo_title": f.get("seo_title"), "seo_description": f.get("seo_description"),
        "rules": rules, "match_all": f.get("match") != "any",
    });
    let handle = f.get("handle");
    if !handle.is_empty() {
        b["handle"] = json!(handle);
    }
    Ok(b)
}

/// The form as typed, to show again with a problem.
fn typed(f: &Fields, id: Option<&str>, currency: &str) -> Value {
    let rules = parse_rules(f, currency).unwrap_or_default();
    json!({
        "id": id, "title": f.get("title"), "handle": f.get("handle"),
        "description": f.get("description"), "seo_title": f.get("seo_title"),
        "seo_description": f.get("seo_description"),
        "rules": if f.get("kind") == "smart" { json!(rules) } else { Value::Null },
        "match_all": f.get("match") != "any",
    })
}

/// Create a collection (a form post).
#[post("/admin/{shop}/collections", tag = "admin")]
pub(crate) async fn create(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path(shop): Path<String>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let f = Fields::parse(&raw);
    let currency = currency(&cx).await;
    let b = match body(&f, &currency) {
        Ok(b) => b,
        Err(e) => {
            let t = typed(&f, None, &currency);
            return Ok(show(cx, t, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await);
        }
    };
    let (status, out) = cx.call(Method::POST, "/api/collections", Some(&b)).await;
    Ok(match status {
        201 => Redirect::to(&format!(
            "/admin/{shop}/collections/{}",
            out["id"].as_str().unwrap_or_default()
        ))
        .into_response(),
        401 => crate::to_login(),
        409 | 422 => {
            let t = typed(&f, None, &currency);
            show(cx, t, Some(message(&out)), StatusCode::UNPROCESSABLE_ENTITY).await
        }
        _ => cx.failed(status, &out),
    })
}

/// Save a collection (a form post).
#[post("/admin/{shop}/collections/{id}", tag = "admin")]
pub(crate) async fn update(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id)): Path<(String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let f = Fields::parse(&raw);
    let currency = currency(&cx).await;
    let b = match body(&f, &currency) {
        Ok(b) => b,
        Err(e) => {
            let t = typed(&f, Some(&id), &currency);
            return Ok(show(cx, t, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await);
        }
    };
    let path = format!("/api/collections/{}", encode(&id));
    let (status, out) = cx.call(Method::PATCH, &path, Some(&b)).await;
    Ok(match status {
        200 => Redirect::to(&format!("/admin/{shop}/collections/{}", encode(&id))).into_response(),
        401 => crate::to_login(),
        409 | 422 => {
            let t = typed(&f, Some(&id), &currency);
            show(cx, t, Some(message(&out)), StatusCode::UNPROCESSABLE_ENTITY).await
        }
        _ => cx.failed(status, &out),
    })
}

/// Add a product by SKU, move one, take one out, or delete the
/// collection (form posts).
#[post("/admin/{shop}/collections/{id}/{action}", tag = "admin")]
pub(crate) async fn act(
    s: State<Shared>,
    db: Db,
    headers: HeaderMap,
    conn: ConnInfo,
    Path((shop, id, action)): Path<(String, String, String)>,
    raw: Bytes,
) -> Result<Response> {
    let cx = page!(Ctx::open(s.0, db, &headers, conn, &shop, "products").await);
    let f = Fields::parse(&raw);
    let base = format!("/api/collections/{}", encode(&id));
    let product = f.get("product");
    let (status, out) = match action.as_str() {
        "delete" => {
            let (status, out) = cx.call(Method::DELETE, &base, None).await;
            if status == 204 {
                return Ok(Redirect::to(&format!("/admin/{shop}/collections")).into_response());
            }
            (status, out)
        }
        "add" => {
            let sku = f.get("sku");
            let found = page!(
                cx.get(&format!("/api/products?q={}&limit=50", encode(&sku)))
                    .await
            );
            let hit = found["items"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| {
                    p["sku"]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(&sku))
                })
                .and_then(|p| p["id"].as_str());
            let Some(pid) = hit else {
                let c = page!(cx.get(&base).await);
                let e = format!("No product has the SKU {sku}.");
                return Ok(show(cx, c, Some(e), StatusCode::UNPROCESSABLE_ENTITY).await);
            };
            cx.call(
                Method::PUT,
                &format!("{base}/products/{}", encode(pid)),
                None,
            )
            .await
        }
        "remove" => {
            cx.call(
                Method::DELETE,
                &format!("{base}/products/{}", encode(&product)),
                None,
            )
            .await
        }
        "up" | "down" => {
            let listed = page!(cx.get(&format!("{base}/products?limit=200")).await);
            let mut ids: Vec<String> = listed["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["id"].as_str().map(str::to_owned))
                .collect();
            if let Some(at) = ids.iter().position(|p| *p == product) {
                match action.as_str() {
                    "up" if at > 0 => ids.swap(at, at - 1),
                    "down" if at + 1 < ids.len() => ids.swap(at, at + 1),
                    _ => {}
                }
            }
            cx.call(
                Method::PUT,
                &format!("{base}/order"),
                Some(&json!({ "product_ids": ids })),
            )
            .await
        }
        _ => return Ok(cx.failed(404, &Value::Null)),
    };
    Ok(match status {
        200..=299 => Redirect::to(&format!(
            "/admin/{shop}/collections/{}#products",
            encode(&id)
        ))
        .into_response(),
        401 => crate::to_login(),
        409 | 422 => {
            let c = page!(cx.get(&base).await);
            show(cx, c, Some(message(&out)), StatusCode::UNPROCESSABLE_ENTITY).await
        }
        _ => cx.failed(status, &out),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_turn_into_rows_and_back() {
        let rules = json!([{ "field": "price_minor", "op": "less_than", "value": "5000" }]);
        let r = rows(&rules, "EUR");
        assert_eq!(r.len(), ROWS);
        assert_eq!(r[0]["value"], "50.00");
        assert!(r[0]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["key"] == "price_minor" && f["selected"] == true));
        assert_eq!(r[1]["value"], "");
        let f = Fields::parse(
            b"field0=price_minor&op0=less_than&value0=50%2C00&field1=tag&op1=equals&value1=",
        );
        let back = parse_rules(&f, "EUR").unwrap();
        assert_eq!(
            back,
            vec![json!({ "field": "price_minor", "op": "less_than", "value": "5000" })]
        );
        assert!(parse_rules(
            &Fields::parse(b"field0=price_minor&op0=equals&value0=abc"),
            "EUR"
        )
        .is_err());
    }
}
